use crate::{
    error::{InvalidRequestReason, NetworkReason, ResponsError},
    network::{Network, QueryParameter, RequestHandle, RequestOptions, ServiceRequest},
};
use http::{Method, Request, Response, StatusCode, header, uri::PathAndQuery};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

type HttpResponse = Response<Full<Bytes>>;
pub type HttpDispatch = dyn Fn(u64, &str, &str) -> bool + Send + Sync;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub struct HttpRoutes {
    handlers: Mutex<HashMap<String, Arc<HttpRoute>>>,
}

struct HttpRoute {
    methods: Vec<Method>,
    allow: String,
    dispatch: Box<HttpDispatch>,
}

impl HttpRoutes {
    pub fn register(
        &self,
        path: &str,
        methods: &[String],
        dispatch: Box<HttpDispatch>,
    ) -> Result<(), String> {
        let path_and_query = path
            .parse::<PathAndQuery>()
            .map_err(|_| "调试 HTTP 路径无效")?;
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['?', '#'])
            || path_and_query.path() != path
        {
            return Err("调试 HTTP 路径必须是绝对路径".into());
        }
        if path.starts_with("/service/") {
            return Err("路径已由调试 HTTP 服务使用".into());
        }
        if methods.is_empty() {
            return Err("请指定至少一个 HTTP 方法".into());
        }
        let methods = methods
            .iter()
            .map(|method| Method::from_bytes(method.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "调试 HTTP 方法无效")?;
        let allow = methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let route = Arc::new(HttpRoute {
            methods,
            allow,
            dispatch,
        });
        self.handlers.lock().unwrap().insert(path.to_owned(), route);
        Ok(())
    }

    pub fn clear(&self) {
        self.handlers.lock().unwrap().clear();
    }

    fn find(&self, path: &str) -> Option<Arc<HttpRoute>> {
        self.handlers.lock().unwrap().get(path).cloned()
    }
}

pub struct HttpServer {
    pub address: SocketAddr,
    ui: Arc<HttpBridge>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl HttpServer {
    pub fn start(
        runtime: &tokio::runtime::Handle,
        network: Arc<Network>,
        port: u16,
        routes: Arc<HttpRoutes>,
    ) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let _runtime = runtime.enter();
        let listener = TcpListener::from_std(listener)?;
        let ui = Arc::new(HttpBridge {
            routes,
            pending: Mutex::new(HashMap::new()),
        });
        let (shutdown, stopped) = oneshot::channel();
        let task = runtime.spawn(serve(listener, network, ui.clone(), stopped));
        Ok(Self {
            address,
            ui,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub fn is_running(&self) -> bool {
        self.shutdown.is_some() && !self.task.is_finished()
    }

    pub fn respond(&self, id: u64, status: u16, body: &str) {
        if let Some(sender) = self.ui.pending.lock().unwrap().remove(&id) {
            let status = StatusCode::from_u16(status)
                .ok()
                .filter(|status| !status.is_informational() && status.as_u16() < 600);
            let response = match (status, serde_json::from_str::<Value>(body)) {
                (Some(status), Ok(body)) => json_response(status, body),
                _ => error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internalError",
                    "界面响应格式无效",
                ),
            };
            let _ = sender.send(response);
        }
    }

    pub fn begin_stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    pub async fn stop(mut self) {
        self.begin_stop();
        let _ = (&mut self.task).await;
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    listener: TcpListener,
    network: Arc<Network>,
    ui: Arc<HttpBridge>,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut stopped => break,
            _ = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept(), if connections.len() < 32 => {
                let Ok((stream, _)) = accepted else { break };
                let network = network.clone();
                let ui = ui.clone();
                connections.spawn(async move {
                    let handler = service_fn(move |request| request_http(request, network.clone(), ui.clone()));
                    let _ = http1::Builder::new()
                        .keep_alive(false)
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(5))
                        .serve_connection(TokioIo::new(stream), handler)
                        .await;
                });
            }
        }
    }
    drop(listener);
    connections.shutdown().await;
}

async fn request_http(
    request: Request<Incoming>,
    network: Arc<Network>,
    ui: Arc<HttpBridge>,
) -> Result<HttpResponse, Infallible> {
    let path = request.uri().path();
    let response = if let Some(path) = path
        .strip_prefix("/service/")
        .filter(|path| !path.is_empty())
    {
        if request.method() == Method::GET {
            let mut query = ServiceRequest::get(format!("/{path}"));
            query.query =
                url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
                    .map(|(name, value)| QueryParameter {
                        name: name.into_owned(),
                        value: value.into_owned(),
                    })
                    .collect();
            query_service(&network, query).await
        } else {
            method_not_allowed("GET")
        }
    } else if let Some(route) = ui.routes.find(path) {
        if route.methods.contains(request.method()) {
            let method = request.method().clone();
            match read_body(request).await {
                Ok(body) => ui.request(&route, method.as_str(), &body).await,
                Err(response) => response,
            }
        } else {
            method_not_allowed(&route.allow)
        }
    } else {
        error(StatusCode::NOT_FOUND, "notFound", "接口不存在")
    };
    Ok(response)
}

async fn read_body(request: Request<Incoming>) -> Result<String, HttpResponse> {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return Ok(String::new());
    }
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(request.into_body(), 8192).collect(),
    )
    .await;
    match body {
        Ok(Ok(body)) => String::from_utf8(body.to_bytes().to_vec()).map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                "invalidInput",
                "请求体必须为 UTF-8",
            )
        }),
        Ok(Err(failure)) if failure.is::<http_body_util::LengthLimitError>() => Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalidInput",
            "请求过大",
        )),
        Ok(Err(_)) => Err(error(
            StatusCode::BAD_REQUEST,
            "invalidInput",
            "读取请求体失败",
        )),
        Err(_) => Err(error(
            StatusCode::REQUEST_TIMEOUT,
            "unavailable",
            "读取请求体超时",
        )),
    }
}

async fn query_service(network: &Network, request: ServiceRequest) -> HttpResponse {
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let success = sender.clone();
    let _cancel = CancelOnDrop(network.request(
        request,
        RequestOptions::default(),
        move |response| {
            if let Some(sender) = success.lock().unwrap().take() {
                let _ = sender.send(json_response(response.status, response.data));
            }
        },
        move |failure| {
            if let Some(sender) = sender.lock().unwrap().take() {
                let _ = sender.send(request_error(failure));
            }
        },
    ));
    receiver.await.unwrap_or_else(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "服务请求已停止",
        )
    })
}

fn request_error(failure: ResponsError) -> HttpResponse {
    let status = match &failure {
        ResponsError::NotConfigured(_) => StatusCode::SERVICE_UNAVAILABLE,
        ResponsError::NotBound(_) => StatusCode::FORBIDDEN,
        ResponsError::Cancelled(_) => StatusCode::SERVICE_UNAVAILABLE,
        ResponsError::Services(failure) => {
            StatusCode::from_u16(failure.status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        ResponsError::Network(failure) => match failure.reason {
            NetworkReason::ConnectTimeout | NetworkReason::RequestTimeout => {
                StatusCode::GATEWAY_TIMEOUT
            }
            _ => StatusCode::BAD_GATEWAY,
        },
        ResponsError::InvalidRequest(failure)
            if failure.reason == InvalidRequestReason::RequestTooLarge =>
        {
            StatusCode::PAYLOAD_TOO_LARGE
        }
        ResponsError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
    };
    let body = serde_json::to_value(failure).expect("请求错误可序列化为 JSON");
    json_response(status, body)
}

struct CancelOnDrop(RequestHandle);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct HttpBridge {
    routes: Arc<HttpRoutes>,
    pending: Mutex<HashMap<u64, oneshot::Sender<HttpResponse>>>,
}

impl HttpBridge {
    async fn request(&self, route: &HttpRoute, method: &str, body: &str) -> HttpResponse {
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let _cleanup = PendingResponse { bridge: self, id };
        if (route.dispatch)(id, method, body) {
            if let Ok(Ok(response)) = tokio::time::timeout(Duration::from_secs(15), receiver).await
            {
                return response;
            }
        }
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "主窗口尚未就绪或请求超时",
        )
    }
}

struct PendingResponse<'a> {
    bridge: &'a HttpBridge,
    id: u64,
}
impl Drop for PendingResponse<'_> {
    fn drop(&mut self) {
        self.bridge.pending.lock().unwrap().remove(&self.id);
    }
}

fn method_not_allowed(allowed: &str) -> HttpResponse {
    let mut response = error(
        StatusCode::METHOD_NOT_ALLOWED,
        "invalidInput",
        "不支持此请求方法",
    );
    response.headers_mut().insert(
        header::ALLOW,
        header::HeaderValue::from_str(allowed).expect("已验证的 HTTP 方法"),
    );
    response
}

fn error(status: StatusCode, code: &str, message: &str) -> HttpResponse {
    json_response(status, json!({"error": {"code": code, "message": message}}))
}

fn json_response(status: StatusCode, body: Value) -> HttpResponse {
    if status == StatusCode::NO_CONTENT || status == StatusCode::RESET_CONTENT {
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = status;
        return response;
    }
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}
