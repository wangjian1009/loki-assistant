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
    http::{HeaderName, HeaderValue, Method, Request, StatusCode, header::ACCEPT},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::StreamExt;
use iroh::{
    EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    sync::Semaphore,
    time::{Instant, timeout, timeout_at},
};
use tower::ServiceExt;

use super::{relay::RelaySession, service::RelayContext};
use crate::{
    http::{
        error::{ErrorSource, response as api_error},
        sse::{self, FramedSse, RemoteSse},
    },
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
    pub response_mode: ResponseMode,
}
pub type DispatchFuture = Pin<Box<dyn Future<Output = Response> + Send>>;
pub trait RequestDispatcher: Send + Sync {
    fn dispatch(&self, context: RequestContext, request: DispatchRequest) -> DispatchFuture;
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    #[default]
    Json,
    Sse,
}

#[derive(Clone)]
pub(crate) struct AuthenticatedPeer(pub(super) EndpointId);

const DEVICE_HEADER: HeaderName = HeaderName::from_static("x-loki-device");
const REMOTE_DEVICES_HEADER: HeaderName = HeaderName::from_static("x-loki-remote-devices");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const SSE_HEADER_LIMIT: usize = 16_384;
const JSON_BODY_LIMIT: usize = 4_000_000;

pub(crate) async fn strip_device_headers(mut request: Request<Body>, next: Next) -> Response {
    request.headers_mut().remove(DEVICE_HEADER);
    request.headers_mut().remove(REMOTE_DEVICES_HEADER);
    next.run(request).await
}

use crate::process::Process;

pub(super) struct Gateway {
    dispatcher: Process<Option<Arc<dyn RequestDispatcher>>>,
    policy: Router,
    total: RequestLimits,
    peers: Process<BTreeMap<EndpointId, Weak<RequestLimits>>>,
}

struct RequestLimits {
    json: Semaphore,
    sse: Semaphore,
}

impl RequestLimits {
    fn new(json: usize, sse: usize) -> Self {
        Self {
            json: Semaphore::new(json),
            sse: Semaphore::new(sse),
        }
    }

    fn for_mode(&self, mode: ResponseMode) -> &Semaphore {
        match mode {
            ResponseMode::Json => &self.json,
            ResponseMode::Sse => &self.sse,
        }
    }
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

fn sse_not_supported() -> Response {
    error(
        StatusCode::NOT_ACCEPTABLE,
        "sse-not-supported",
        "此接口不支持 SSE 响应",
    )
}

impl Gateway {
    pub fn new() -> Self {
        let mut paths: BTreeMap<&str, BTreeMap<&str, (&str, bool)>> = BTreeMap::new();
        for api in API_DESCRIPTORS {
            paths
                .entry(api.path)
                .or_default()
                .insert(api.method, (api.access, api.sse_event.is_some()));
        }
        let mut policy = Router::new().fallback(|| async {
            error(StatusCode::NOT_FOUND, "api-not-found", "未找到接口")
        });
        for (path, methods) in paths {
            policy = policy.route(
                path,
                any(
                    move |method: Method, mode: Option<axum::Extension<RemoteSse>>| {
                        let access = methods
                            .get(method.as_str())
                            .or_else(|| methods.get("ANY"))
                            .copied();
                        async move {
                            match access {
                                Some(("paired", false)) if mode.is_some() => sse_not_supported(),
                                Some(("paired", _)) => StatusCode::NO_CONTENT.into_response(),
                                Some(_) => remote_api_forbidden(),
                                None => error(
                                    StatusCode::METHOD_NOT_ALLOWED,
                                    "method-not-allowed",
                                    "此接口不支持该请求方法",
                                ),
                            }
                        }
                    },
                ),
            );
        }
        Self {
            dispatcher: Process::start(None),
            policy,
            total: RequestLimits::new(64, 16),
            peers: Process::start(BTreeMap::new()),
        }
    }

    async fn peer_limit(&self, id: EndpointId) -> Arc<RequestLimits> {
        self.peers
            .call(move |peers| {
                peers.retain(|_, limit| limit.strong_count() > 0);
                if let Some(limit) = peers.get(&id).and_then(Weak::upgrade) {
                    return limit;
                }
                let limit = Arc::new(RequestLimits::new(8, 4));
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
    #[serde(default)]
    response_mode: ResponseMode,
}

#[derive(Serialize)]
struct SseResponseHead<'a> {
    api_version: &'static str,
    request_id: Option<&'a str>,
    status: u64,
    content_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
}

impl Gateway {
    pub(super) fn set_dispatcher(&self, dispatcher: Arc<dyn RequestDispatcher>) {
        self.dispatcher
            .post(move |current| *current = Some(dispatcher));
    }
}

impl RelayContext {
    pub(super) async fn handle_remote(
        &self,
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
                    let Ok((send, recv)) = stream else { break; };
                    let service = self.clone();
                    let session = session.clone();
                    let connection = connection.clone();
                    let limit = limit.clone();
                    tasks.spawn(async move {
                        service.handle_request(&session, &connection, &limit, send, recv).await;
                    });
                }
            }
        }
    }

    async fn handle_request(
        &self,
        session: &Arc<dyn RelaySession>,
        connection: &Connection,
        limit: &RequestLimits,
        mut send: SendStream,
        mut recv: RecvStream,
    ) {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let bytes = match timeout_at(deadline, recv.read_to_end(1_048_576)).await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => {
                let response = error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request-too-large",
                    "请求超过大小限制",
                );
                let _ = timeout_at(deadline, send_json_response(&mut send, None, response)).await;
                return;
            }
            Err(_) => {
                let _ = send.reset(1u8.into());
                return;
            }
        };
        let request = serde_json::from_slice::<WireRequest>(&bytes).ok();
        let mode = request
            .as_ref()
            .map(|request| request.response_mode)
            .unwrap_or_default();
        let request_id = request
            .as_ref()
            .and_then(|request| uuid::Uuid::parse_str(&request.request_id).ok())
            .map(|id| id.to_string());
        let total = self.remote.total.for_mode(mode).try_acquire();
        let peer = limit.for_mode(mode).try_acquire();
        let response = if total.is_err() || peer.is_err() {
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "too-many-requests",
                "同时处理的请求过多",
            )
        } else if let Some(request) = request.filter(|request| {
            request.api_version == "1" && request_id.as_deref() == Some(request.request_id.as_str())
        }) {
            tokio::select! {
                response = timeout_at(deadline, self.dispatch_remote(session, connection, request)) => {
                    match response {
                        Ok(response) => response,
                        Err(_) => {
                            let _ = send.reset(1u8.into());
                            return;
                        }
                    }
                }
                _ = send.stopped() => return,
            }
        } else {
            error(
                StatusCode::BAD_REQUEST,
                "invalid-request",
                "请求格式或协议版本无效",
            )
        };

        match mode {
            ResponseMode::Json => {
                let _ = timeout_at(
                    deadline,
                    send_json_response(&mut send, request_id, response),
                )
                .await;
            }
            ResponseMode::Sse => {
                if !send_sse_response(&mut send, request_id, response, deadline).await {
                    let _ = send.reset(1u8.into());
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
        let mut probe = match Request::builder()
            .method(method.clone())
            .uri(&uri)
            .body(Body::empty())
        {
            Ok(probe) => probe,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid-request", "接口路径无效"),
        };
        if request.response_mode == ResponseMode::Sse {
            probe.extensions_mut().insert(RemoteSse);
        }
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
            response_mode: request.response_mode,
        };
        dispatcher.dispatch(context, request).await
    }
}

async fn send_json_response(send: &mut SendStream, request_id: Option<String>, response: Response) {
    let (status, body) = json_body(response, JSON_BODY_LIMIT).await;
    let encoded = serde_json::to_vec(&json!({
        "api_version": "1", "request_id": request_id, "status": status, "body": body
    }))
    .unwrap();
    if send.write_all(&encoded).await.is_ok() && send.finish().is_ok() {
        let _ = send.stopped().await;
    }
}

async fn send_sse_response(
    send: &mut SendStream,
    request_id: Option<String>,
    mut response: Response,
    deadline: Instant,
) -> bool {
    if response.status().is_success() && response.extensions().get::<FramedSse>().is_some() {
        let header = SseResponseHead {
            api_version: "1",
            request_id: request_id.as_deref(),
            status: u64::from(response.status().as_u16()),
            content_type: sse::MEDIA_TYPE,
            body: None,
        };
        let header = sse::encode_frame(&header, SSE_HEADER_LIMIT).unwrap();
        if !timeout_at(deadline, send.write_all(&header))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return false;
        }
        let mut frames = response.into_body().into_data_stream();
        let mut stopped = std::pin::pin!(send.stopped());
        loop {
            let bytes = tokio::select! {
                frame = frames.next() => match frame {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(_)) => return false,
                    None => return send.finish().is_ok(),
                },
                _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => axum::body::Bytes::from_static(&[0; 4]),
                _ = &mut stopped => return false,
            };
            if !timeout(REQUEST_TIMEOUT, send.write_all(&bytes))
                .await
                .is_ok_and(|result| result.is_ok())
            {
                return false;
            }
        }
    }
    if !(response.status().is_client_error() || response.status().is_server_error()) {
        response = error(
            StatusCode::BAD_GATEWAY,
            "invalid-service-response",
            "服务返回了无效 SSE 响应",
        );
    }
    timeout_at(deadline, async {
        let (status, body) = json_body(response, SSE_HEADER_LIMIT).await;
        let mut header = SseResponseHead {
            api_version: "1",
            request_id: request_id.as_deref(),
            status,
            content_type: "application/json",
            body: Some(body),
        };
        let frame = sse::encode_frame(&header, SSE_HEADER_LIMIT).unwrap_or_else(|| {
            header.status = 502;
            header.body = Some(response_too_large());
            sse::encode_frame(&header, SSE_HEADER_LIMIT).unwrap()
        });
        send.write_all(&frame).await.is_ok() && send.finish().is_ok()
    })
    .await
    .unwrap_or(false)
}

async fn json_body(response: Response, max_len: usize) -> (u64, Value) {
    let status = u64::from(response.status().as_u16());
    if status == 204 {
        return (status, Value::Null);
    }
    let body = match to_bytes(response.into_body(), max_len).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            json!({
                "api_version": "1", "status": 502,
                "error": {"source":"services", "code":"invalid-service-response", "message":"服务返回了无效 JSON"}
            })
        }),
        Err(_) => response_too_large(),
    };
    let status = body
        .get("status")
        .and_then(Value::as_u64)
        .filter(|_| body.get("error").is_some())
        .unwrap_or(status);
    (status, body)
}

fn response_too_large() -> Value {
    json!({
        "api_version": "1", "status": 502,
        "error": {"source":"services", "code":"response-too-large", "message":"服务响应超过大小限制"}
    })
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
    let api = API_DESCRIPTORS.iter().find(|api| {
        api.path == path.as_str()
            && api.method == request.method().as_str()
            && api.access == "paired"
    });
    let Some(api) = api else {
        return remote_api_forbidden();
    };
    if request.extensions().get::<RemoteSse>().is_some() && api.sse_event.is_none() {
        return sse_not_supported();
    }
    next.run(request).await
}

impl RequestDispatcher for HttpDispatcher {
    fn dispatch(&self, context: RequestContext, request: DispatchRequest) -> DispatchFuture {
        let router = self.router.clone();
        Box::pin(async move {
            let Some(router) = router() else {
                return error(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "service-closed",
                    "服务已关闭",
                );
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
            http.extensions_mut().insert(crate::http::RemoteRequest);
            if request.response_mode == ResponseMode::Sse {
                http.headers_mut()
                    .insert(ACCEPT, HeaderValue::from_static(sse::MEDIA_TYPE));
                http.extensions_mut().insert(RemoteSse);
            }
            http.extensions_mut()
                .insert(AuthenticatedPeer(context.peer_id));
            let router = router.layer(middleware::from_fn(authorize_http_route));
            router.oneshot(http).await.unwrap()
        })
    }
}
