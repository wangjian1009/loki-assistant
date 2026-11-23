use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::MatchedPath,
    http::{HeaderName, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use iroh::{EndpointId, endpoint::Connection};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use super::{NetworkService, relay::RelaySession};
use crate::{
    http::error::{ErrorSource, response as api_error},
    modules::discover::registry::API_DESCRIPTORS,
};

pub struct RequestContext {
    pub request_id: uuid::Uuid,
    pub peer_id: EndpointId,
}

pub struct QueryParameter {
    pub name: String,
    pub value: String,
}
pub struct DispatchRequest {
    pub method: Method,
    pub path: String,
    pub query: Vec<QueryParameter>,
    pub body: Option<Value>,
}
pub struct DispatchResponse {
    pub status: StatusCode,
    pub body: Value,
}
pub type DispatchFuture = Pin<Box<dyn Future<Output = DispatchResponse> + Send>>;
pub trait RequestDispatcher: Send + Sync {
    fn dispatch(&self, context: RequestContext, request: DispatchRequest) -> DispatchFuture;
}

impl DispatchResponse {
    pub async fn from_http(response: Response) -> Self {
        let status = response.status();
        let body = response_body(response).await;
        let status = body
            .get("status")
            .and_then(Value::as_u64)
            .filter(|_| body.get("error").is_some())
            .and_then(|status| u16::try_from(status).ok())
            .and_then(|status| StatusCode::from_u16(status).ok())
            .unwrap_or(status);
        Self { status, body }
    }
}

#[derive(Clone)]
pub(crate) struct AuthenticatedPeer(pub(super) EndpointId);

const DEVICE_HEADER: HeaderName = HeaderName::from_static("x-loki-device");
const REMOTE_DEVICES_HEADER: HeaderName = HeaderName::from_static("x-loki-remote-devices");

pub(crate) async fn strip_device_headers(mut request: Request<Body>, next: Next) -> Response {
    request.headers_mut().remove(DEVICE_HEADER);
    request.headers_mut().remove(REMOTE_DEVICES_HEADER);
    next.run(request).await
}

use crate::process::Process;

pub(super) struct Gateway {
    dispatcher: Process<Option<Arc<dyn RequestDispatcher>>>,
    policy: Router,
    total: Arc<Semaphore>,
    peers: Process<BTreeMap<EndpointId, Weak<Semaphore>>>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway")
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    api_error(status, ErrorSource::Services, code, message)
}

fn remote_api_forbidden() -> Response {
    error(
        StatusCode::FORBIDDEN,
        "remote-api-forbidden",
        "此接口仅限电脑本机调用",
    )
}

impl Gateway {
    pub fn new() -> Self {
        let mut paths: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
        for api in API_DESCRIPTORS {
            paths
                .entry(api.path)
                .or_default()
                .insert(api.method, api.access);
        }
        let mut policy = Router::new().fallback(|| async {
            error(StatusCode::NOT_FOUND, "api-not-found", "未找到接口")
        });
        for (path, methods) in paths {
            policy = policy.route(
                path,
                any(move |method: Method| {
                    let access = methods.get(method.as_str()).copied();
                    async move {
                        match access {
                            Some("paired") => StatusCode::NO_CONTENT.into_response(),
                            Some(_) => remote_api_forbidden(),
                            None => error(
                                StatusCode::METHOD_NOT_ALLOWED,
                                "method-not-allowed",
                                "此接口不支持该请求方法",
                            ),
                        }
                    }
                }),
            );
        }
        Self {
            dispatcher: Process::start(None),
            policy,
            total: Arc::new(Semaphore::new(64)),
            peers: Process::start(BTreeMap::new()),
        }
    }

    async fn peer_limit(&self, id: EndpointId) -> Arc<Semaphore> {
        self.peers
            .call(move |peers| {
                peers.retain(|_, limit| limit.strong_count() > 0);
                if let Some(limit) = peers.get(&id).and_then(Weak::upgrade) {
                    return limit;
                }
                let limit = Arc::new(Semaphore::new(8));
                peers.insert(id, Arc::downgrade(&limit));
                limit
            })
            .await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    name: String,
    value: String,
}

fn present_body<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    api_version: String,
    request_id: String,
    #[serde(default)]
    binding_token: String,
    method: String,
    path: String,
    #[serde(default)]
    query: Vec<Query>,
    #[serde(default, deserialize_with = "present_body")]
    body: Option<Value>,
}

impl NetworkService {
    /// 组合根注入只捕获 Weak 引用的分发器，避免服务实例形成循环持有。
    pub fn set_remote_dispatcher(&self, dispatcher: Arc<dyn RequestDispatcher>) {
        self.remote
            .dispatcher
            .post(move |current| *current = Some(dispatcher));
    }

    pub(super) async fn handle_remote(
        self: Arc<Self>,
        session: &Arc<dyn RelaySession>,
        connection: &Connection,
    ) {
        let limit = self.remote.peer_limit(connection.remote_id()).await;
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = connection.closed() => break,
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                stream = connection.accept_bi(), if tasks.len() < 16 => {
                    let Ok((mut send, mut recv)) = stream else { break; };
                    let service = self.clone();
                    let session = session.clone();
                    let connection = connection.clone();
                    let limit = limit.clone();
                    tasks.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(30), async {
                            let bytes = match recv.read_to_end(1_048_576).await {
                                Ok(bytes) => bytes,
                                Err(_) => {
                                    let body = response_body(error(StatusCode::PAYLOAD_TOO_LARGE, "request-too-large", "请求超过大小限制")).await;
                                    let _ = send.write_all(&serde_json::to_vec(&json!({"api_version":"1","request_id":null,"status":413,"body":body})).unwrap()).await;
                                    let _ = send.finish();
                                    return;
                                }
                            };
                            let request = serde_json::from_slice::<WireRequest>(&bytes).ok();
                            let request_id = request.as_ref().and_then(|r| uuid::Uuid::parse_str(&r.request_id).ok()).map(|id| id.to_string());
                            let total = service.remote.total.clone().try_acquire_owned();
                            let peer = limit.try_acquire_owned();
                            let response = if total.is_err() || peer.is_err() {
                                error(StatusCode::TOO_MANY_REQUESTS, "too-many-requests", "同时处理的请求过多")
                            } else if let Some(request) = request.filter(|r| r.api_version == "1" && request_id.as_deref() == Some(r.request_id.as_str())) {
                                service.dispatch_remote(&session, &connection, request).await
                            } else {
                                error(StatusCode::BAD_REQUEST, "invalid-request", "请求格式或协议版本无效")
                            };
                            let status = response.status().as_u16();
                            let body = response_body(response).await;
                            let status = body.get("status").and_then(Value::as_u64).filter(|_| body.get("error").is_some()).unwrap_or(status as u64);
                            let encoded = serde_json::to_vec(&json!({"api_version":"1","request_id":request_id,"status":status,"body":body})).unwrap();
                            let _ = send.write_all(&encoded).await;
                            let _ = send.finish();
                            let _ = send.stopped().await;
                        }).await;
                    });
                }
            }
        }
    }

    async fn dispatch_remote(
        &self,
        session: &Arc<dyn RelaySession>,
        connection: &Connection,
        request: WireRequest,
    ) -> Response {
        if let Err(reason) = self
            .authenticated_peer(session, connection.remote_id())
            .await
        {
            return api_error(
                StatusCode::from_u16(reason.status()).unwrap(),
                ErrorSource::Business,
                reason.code(),
                reason.to_string(),
            );
        }
        if let Err(reason) = self
            .pairings
            .authenticate(connection.remote_id(), &request.binding_token)
            .await
        {
            return api_error(
                StatusCode::from_u16(reason.status()).unwrap(),
                ErrorSource::Business,
                reason.code(),
                reason.to_string(),
            );
        }
        if !request.path.starts_with('/')
            || request.path.starts_with("//")
            || request.path.contains(['?', '#', '\\'])
            || request.path.chars().any(char::is_control)
        {
            return error(StatusCode::BAD_REQUEST, "invalid-request", "接口路径无效");
        }
        let method: Method = match request.method.parse() {
            Ok(method) => method,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid-request", "请求方法无效"),
        };
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(request.query.iter().map(|q| (&q.name, &q.value)))
            .finish();
        let uri = if query.is_empty() {
            request.path
        } else {
            format!("{}?{query}", request.path)
        };
        let probe = match Request::builder()
            .method(method.clone())
            .uri(&uri)
            .body(Body::empty())
        {
            Ok(probe) => probe,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid-request", "接口路径无效"),
        };
        let authorized = self.remote.policy.clone().oneshot(probe).await.unwrap();
        if authorized.status() != StatusCode::NO_CONTENT {
            return authorized;
        }
        let dispatcher = self
            .remote
            .dispatcher
            .call(|dispatcher| dispatcher.clone())
            .await;
        let Some(dispatcher) = dispatcher else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "network-not-ready",
                "服务分发尚未就绪",
            );
        };
        let context = RequestContext {
            request_id: uuid::Uuid::parse_str(&request.request_id).unwrap(),
            peer_id: connection.remote_id(),
        };
        let request = DispatchRequest {
            method,
            path: uri.split('?').next().unwrap().to_owned(),
            query: request
                .query
                .into_iter()
                .map(|q| QueryParameter {
                    name: q.name,
                    value: q.value,
                })
                .collect(),
            body: request.body,
        };
        let response = dispatcher.dispatch(context, request).await;
        (response.status, axum::Json(response.body)).into_response()
    }
}

async fn response_body(response: Response) -> Value {
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Value::Null;
    }
    match to_bytes(response.into_body(), 4_000_000).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({"api_version":"1","status":502,"error":{"source":"services","code":"invalid-service-response","message":"服务返回了无效 JSON"}})),
        Err(_) => json!({"api_version":"1","status":502,"error":{"source":"services","code":"response-too-large","message":"服务响应超过大小限制"}}),
    }
}

pub struct HttpDispatcher {
    router: Arc<dyn Fn() -> Option<Router> + Send + Sync>,
}
impl HttpDispatcher {
    pub fn new(router: impl Fn() -> Option<Router> + Send + Sync + 'static) -> Self {
        Self {
            router: Arc::new(router),
        }
    }
}

async fn authorize_http_route(request: Request<Body>, next: Next) -> Response {
    let Some(path) = request.extensions().get::<MatchedPath>() else {
        return error(StatusCode::NOT_FOUND, "api-not-found", "未找到接口");
    };
    // 校验实际选中的路由，防止动态路径的访问权限被继承到其他 handler。
    let allowed = API_DESCRIPTORS.iter().any(|api| {
        api.path == path.as_str()
            && api.method == request.method().as_str()
            && api.access == "paired"
    });
    if !allowed {
        return remote_api_forbidden();
    }
    next.run(request).await
}

impl RequestDispatcher for HttpDispatcher {
    fn dispatch(&self, context: RequestContext, request: DispatchRequest) -> DispatchFuture {
        let router = self.router.clone();
        Box::pin(async move {
            let Some(router) = router() else {
                return DispatchResponse::from_http(crate::http::error::response(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    crate::http::error::ErrorSource::Services,
                    "service-closed",
                    "服务已关闭",
                ))
                .await;
            };
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(request.query.iter().map(|q| (&q.name, &q.value)))
                .finish();
            let uri = if query.is_empty() {
                request.path
            } else {
                format!("{}?{query}", request.path)
            };
            let body = request
                .body
                .map(|value| serde_json::to_vec(&value).unwrap())
                .unwrap_or_default();
            let device = HeaderValue::from_str(&context.peer_id.to_string())
                .expect("Endpoint ID 可以作为 HTTP 请求头值");
            let mut http = axum::http::Request::builder()
                .method(request.method)
                .uri(uri)
                .header("content-type", "application/json")
                .header(DEVICE_HEADER, device.clone())
                .header(REMOTE_DEVICES_HEADER, device)
                .body(axum::body::Body::from(body))
                .unwrap();
            http.extensions_mut()
                .insert(AuthenticatedPeer(context.peer_id));
            let router = router.layer(middleware::from_fn(authorize_http_route));
            DispatchResponse::from_http(router.oneshot(http).await.unwrap()).await
        })
    }
}
