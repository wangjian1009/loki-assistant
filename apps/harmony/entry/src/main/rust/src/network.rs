use http::{Method, StatusCode};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, SecretKey,
    endpoint::{Connection, presets},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, watch};
use uuid::Uuid;

pub(crate) const PAIR_ALPN: &[u8] = b"loki-assistant/pair/1";
const SERVICES_ALPN: &[u8] = b"loki-assistant/services/1";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    ServiceNotConfigured,
    ServiceChanged,
    Closed,
    Transport,
    Timeout,
    Cancelled,
    Protocol,
    Api,
    InvalidRequest,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotSent,
    Rejected,
    Unknown,
}
#[derive(Clone, Debug, Serialize)]
pub struct NetworkError {
    pub kind: ErrorKind,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
    pub outcome: Outcome,
    pub status: Option<u16>,
    pub source: Option<String>,
}
impl NetworkError {
    pub(crate) fn new(kind: ErrorKind) -> Self {
        let (code, message) = match kind {
            ErrorKind::ServiceNotConfigured => ("service-not-configured", "尚未设置绑定的电脑服务"),
            ErrorKind::ServiceChanged => ("service-changed", "绑定的电脑服务已切换"),
            ErrorKind::Closed => ("network-closed", "网络模块已关闭"),
            ErrorKind::Transport => (
                "unreachable",
                "暂时无法连接电脑，请确认电脑服务和网络已开启",
            ),
            ErrorKind::Timeout => ("request-timeout", "请求超时，请检查电脑服务和网络"),
            ErrorKind::Cancelled => ("request-cancelled", "请求已取消"),
            ErrorKind::Protocol => (
                "invalid-service-response",
                "电脑服务协议不兼容或响应无效，请更新电脑服务",
            ),
            ErrorKind::InvalidRequest => ("invalid-request", "请求参数无效"),
            ErrorKind::Api => ("service-error", "电脑服务暂时无法处理请求"),
        };
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            request_id: None,
            outcome: Outcome::NotSent,
            status: None,
            source: None,
        }
    }
    pub(crate) fn api(value: &Value, status: u16) -> Result<Self, Self> {
        let e = &value["error"];
        let source = e["source"]
            .as_str()
            .filter(|s| matches!(*s, "services" | "emacs" | "business"))
            .ok_or_else(|| Self::new(ErrorKind::Protocol))?;
        let code = e["code"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Self::new(ErrorKind::Protocol))?;
        let message = e["message"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| Self::new(ErrorKind::Protocol))?;
        if value["api_version"] != "1" || value["status"].as_u64() != Some(status as u64) {
            return Err(Self::new(ErrorKind::Protocol));
        }
        Ok(Self {
            kind: ErrorKind::Api,
            code: code.into(),
            message: message.into(),
            request_id: None,
            outcome: Outcome::Rejected,
            status: Some(status),
            source: Some(source.into()),
        })
    }
}
impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for NetworkError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParameter {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug)]
pub struct ServiceRequest {
    pub method: Method,
    pub path: String,
    pub query: Vec<QueryParameter>,
    pub body: Option<Value>,
}
impl ServiceRequest {
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::GET,
            path: path.into(),
            query: vec![],
            body: None,
        }
    }
}
#[derive(Clone, Copy)]
pub struct RequestOptions {
    pub timeout: Duration,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}
#[derive(Debug)]
pub struct ServiceResponse<T> {
    pub request_id: Uuid,
    pub status: StatusCode,
    pub data: T,
    pub generation: u64,
}

/// 由外部绑定模块构造；凭据均为私有字段，不实现 Debug 或 Serialize。
pub struct Service {
    identity: SecretKey,
    id: EndpointId,
    relay_url: String,
    relay_token: String,
    endpoint: AsyncMutex<Option<Endpoint>>,
    connection: AsyncMutex<Option<Connection>>,
    closed: AtomicBool,
}
impl Service {
    pub fn new(
        identity: SecretKey,
        id: EndpointId,
        relay_url: String,
        relay_token: String,
    ) -> Result<Self, NetworkError> {
        if identity.public() == id || !crate::storage::valid_relay(&relay_url, &relay_token) {
            return Err(NetworkError::new(ErrorKind::InvalidRequest));
        }
        Ok(Self {
            identity,
            id,
            relay_url,
            relay_token,
            endpoint: AsyncMutex::new(None),
            connection: AsyncMutex::new(None),
            closed: AtomicBool::new(false),
        })
    }
    pub fn endpoint_id(&self) -> EndpointId {
        self.id
    }
    fn same_binding(&self, other: &Self) -> bool {
        self.id == other.id
            && self.identity.public() == other.identity.public()
            && self.relay_url == other.relay_url
            && self.relay_token == other.relay_token
    }
    async fn endpoint(&self) -> Result<Endpoint, NetworkError> {
        let mut guard = self.endpoint.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(NetworkError::new(ErrorKind::ServiceChanged));
        }
        if let Some(endpoint) = guard.as_ref() {
            return Ok(endpoint.clone());
        }
        let relay: iroh::RelayUrl = self
            .relay_url
            .parse()
            .map_err(|_| NetworkError::new(ErrorKind::InvalidRequest))?;
        let relays: RelayMap = [relay].into_iter().collect();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(self.identity.clone())
            .clear_ip_transports()
            .relay_mode(RelayMode::Custom(
                relays.with_auth_token(self.relay_token.clone()),
            ))
            .bind()
            .await
            .map_err(|_| NetworkError::new(ErrorKind::Transport))?;
        *guard = Some(endpoint.clone());
        Ok(endpoint)
    }
    async fn connect(&self, alpn: &[u8]) -> Result<Connection, NetworkError> {
        let endpoint = self.endpoint().await?;
        let remote = EndpointAddr::new(self.id).with_relay_url(
            self.relay_url
                .parse()
                .map_err(|_| NetworkError::new(ErrorKind::InvalidRequest))?,
        );
        tokio::time::timeout(Duration::from_secs(10), endpoint.connect(remote, alpn))
            .await
            .map_err(|_| NetworkError::new(ErrorKind::Timeout))?
            .map_err(|_| NetworkError::new(ErrorKind::Transport))
    }
    async fn connection(&self) -> Result<Connection, NetworkError> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref().filter(|c| c.close_reason().is_none()) {
            return Ok(connection.clone());
        }
        let connection = self.connect(SERVICES_ALPN).await?;
        *guard = Some(connection.clone());
        Ok(connection)
    }
    async fn exchange(
        &self,
        request: &ServiceRequest,
        id: Uuid,
        sent: &AtomicBool,
    ) -> Result<(StatusCode, Value), NetworkError> {
        let mut body = json!({"api_version":"1","request_id":id.to_string(),"method":request.method.as_str(),"path":request.path,"query":request.query});
        if let Some(value) = &request.body {
            body["body"] = value.clone();
        }
        let bytes = serde_json::to_vec(&body).unwrap();
        if bytes.len() > 1_048_576 {
            return Err(NetworkError::new(ErrorKind::InvalidRequest));
        }
        let connection = self.connection().await?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| NetworkError::new(ErrorKind::Transport))?;
        sent.store(true, Ordering::SeqCst);
        send.write_all(&bytes)
            .await
            .map_err(|_| NetworkError::new(ErrorKind::Transport))?;
        send.finish()
            .map_err(|_| NetworkError::new(ErrorKind::Transport))?;
        let bytes = recv
            .read_to_end(4_194_304)
            .await
            .map_err(|_| NetworkError::new(ErrorKind::Transport))?;
        let reply: Value =
            serde_json::from_slice(&bytes).map_err(|_| NetworkError::new(ErrorKind::Protocol))?;
        if reply["api_version"] != "1"
            || reply["request_id"] != id.to_string()
            || reply.get("body").is_none()
        {
            return Err(NetworkError::new(ErrorKind::Protocol));
        }
        let status = reply["status"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok())
            .and_then(|s| StatusCode::from_u16(s).ok())
            .filter(|s| s.is_success() || s.is_client_error() || s.is_server_error())
            .ok_or_else(|| NetworkError::new(ErrorKind::Protocol))?;
        if !status.is_success() {
            return Err(NetworkError::api(&reply["body"], status.as_u16())?);
        }
        if status == StatusCode::NO_CONTENT && !reply["body"].is_null() {
            return Err(NetworkError::new(ErrorKind::Protocol));
        }
        Ok((status, reply["body"].clone()))
    }
    pub(crate) async fn pair(
        &self,
        invitation: &crate::storage::Invitation,
    ) -> Result<Value, NetworkError> {
        let result = tokio::time::timeout(Duration::from_secs(20), async {
            let connection = self.connect(PAIR_ALPN).await?;
            let (mut send, mut recv) = connection.open_bi().await.map_err(|_| NetworkError::new(ErrorKind::Transport))?;
            let request = json!({"api_version":"1","type":"pairing.bind","pairing_id":invitation.pairing_id,"pairing_token":invitation.pairing_token,"device_name":"HarmonyOS 手机"});
            send.write_all(&serde_json::to_vec(&request).unwrap()).await.map_err(|_| NetworkError::new(ErrorKind::Transport))?;
            send.finish().map_err(|_| NetworkError::new(ErrorKind::Transport))?;
            let bytes = recv.read_to_end(16_384).await.map_err(|_| NetworkError::new(ErrorKind::Transport))?;
            let reply: Value = serde_json::from_slice(&bytes).map_err(|_| NetworkError::new(ErrorKind::Protocol))?;
            connection.close(0u32.into(), b"finished");
            if let Some(status) = reply["status"].as_u64() { return Err(NetworkError::api(&reply, status as u16)?); }
            if reply["api_version"] != "1" || reply["type"] != "pairing.bound" || reply["endpoint_id"] != self.id.to_string() || reply["bound_at"].as_u64().is_none() { return Err(NetworkError::new(ErrorKind::Protocol)); }
            Ok(reply)
        }).await.map_err(|_| NetworkError::new(ErrorKind::Timeout))?;
        result
    }
    async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(connection) = self.connection.lock().await.take() {
            connection.close(0u32.into(), b"service changed");
        }
        let mut endpoint = self.endpoint.lock().await;
        if let Some(value) = endpoint.take() {
            value.close().await;
        }
    }
}

struct State {
    service: Option<Arc<Service>>,
    retired: Vec<Arc<Service>>,
    generation: u64,
    closed: bool,
    requests: BTreeMap<Uuid, watch::Sender<bool>>,
}
struct Inner {
    retirement: AsyncMutex<()>,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    done: Notify,
    runtime: tokio::runtime::Handle,
}
#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}
pub struct RequestHandle {
    cancel: watch::Sender<bool>,
}
impl RequestHandle {
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }
}
impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}
impl Network {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                retirement: AsyncMutex::new(()),
                state: Mutex::new(State {
                    service: None,
                    retired: vec![],
                    generation: 0,
                    closed: false,
                    requests: BTreeMap::new(),
                }),
                changed: watch::channel(0).0,
                done: Notify::new(),
                runtime: tokio::runtime::Handle::current(),
            }),
        }
    }
    pub fn set_service(&self, service: Service) -> Result<(), NetworkError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.closed {
            return Err(NetworkError::new(ErrorKind::Closed));
        }
        if state
            .service
            .as_ref()
            .is_some_and(|old| old.same_binding(&service))
        {
            return Ok(());
        }
        if let Some(old) = state.service.replace(Arc::new(service)) {
            state.retired.push(old);
        }
        state.generation += 1;
        self.inner.changed.send_replace(state.generation);
        drop(state);
        self.schedule_retirement();
        Ok(())
    }
    pub fn clear_service(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(old) = state.service.take() {
            state.retired.push(old);
        }
        state.generation += 1;
        self.inner.changed.send_replace(state.generation);
        drop(state);
        self.schedule_retirement();
    }
    pub fn has_service(&self) -> bool {
        self.inner.state.lock().unwrap().service.is_some()
    }
    pub fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }
    pub(crate) async fn selected_service(&self) -> Result<Arc<Service>, NetworkError> {
        self.retire().await;
        let service = {
            let state = self.inner.state.lock().unwrap();
            if state.closed {
                return Err(NetworkError::new(ErrorKind::Closed));
            }
            state.service.clone()
        };
        service.ok_or_else(|| NetworkError::new(ErrorKind::ServiceNotConfigured))
    }

    fn schedule_retirement(&self) {
        let network = self.clone();
        self.inner.runtime.spawn(async move {
            network.retire().await;
        });
    }

    async fn retire(&self) {
        let _guard = self.inner.retirement.lock().await;
        let retired = std::mem::take(&mut self.inner.state.lock().unwrap().retired);
        for service in retired {
            service.close().await;
        }
    }
    pub fn request<T, OnSuccess, OnError>(
        &self,
        request: ServiceRequest,
        options: RequestOptions,
        on_success: OnSuccess,
        on_error: OnError,
    ) -> RequestHandle
    where
        T: DeserializeOwned + Send + 'static,
        OnSuccess: FnOnce(ServiceResponse<T>) + Send + 'static,
        OnError: FnOnce(NetworkError) + Send + 'static,
    {
        let id = Uuid::new_v4();
        let (cancel, mut cancelled) = watch::channel(false);
        let (service, generation, closed, mut changed) = {
            let mut state = self.inner.state.lock().unwrap();
            state.requests.insert(id, cancel.clone());
            (
                state.service.clone(),
                state.generation,
                state.closed,
                self.inner.changed.subscribe(),
            )
        };
        // Workspace 已释放运行时后，外部仍可能持有 Arc<Network>。
        // 此时直接交付关闭错误，避免把回调提交到已经停止的执行器。
        if closed {
            self.inner.state.lock().unwrap().requests.remove(&id);
            let mut error = NetworkError::new(ErrorKind::Closed);
            error.request_id = Some(id.to_string());
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_error(error)));
            self.inner.done.notify_waiters();
            return RequestHandle { cancel };
        }
        let network = self.clone();
        self.inner.runtime.spawn(async move {
            let sent = AtomicBool::new(false);
            let mut result = if *cancelled.borrow() { Err(NetworkError::new(ErrorKind::Cancelled)) }
                else if closed { Err(NetworkError::new(ErrorKind::Closed)) }
                else if service.is_none() { Err(NetworkError::new(ErrorKind::ServiceNotConfigured)) }
                else if !request.path.starts_with('/') || request.path.starts_with("//") || request.path.contains(['?', '#', '\\']) || request.path.chars().any(char::is_control) || !(Duration::from_secs(1)..=Duration::from_secs(30)).contains(&options.timeout) {
                    Err(NetworkError::new(ErrorKind::InvalidRequest))
                } else {
                    tokio::select! { biased;
                        _ = async { if !*cancelled.borrow() { let _ = cancelled.changed().await; } } => Err(NetworkError::new(ErrorKind::Cancelled)),
                        _ = async { if *changed.borrow() == generation { let _ = changed.changed().await; } } => Err(NetworkError::new(if network.inner.state.lock().unwrap().closed { ErrorKind::Closed } else { ErrorKind::ServiceChanged })),
                        result = tokio::time::timeout(options.timeout, async {
                            network.selected_service().await?;
                            let (status, value) = service.unwrap().exchange(&request, id, &sent).await?;
                            let data = serde_json::from_value(value).map_err(|_| NetworkError::new(ErrorKind::Protocol))?;
                            Ok(ServiceResponse {request_id: id, status, data, generation})
                        }) => result.unwrap_or_else(|_| Err(NetworkError::new(ErrorKind::Timeout))),
                    }
                };
            if *cancelled.borrow() { result = Err(NetworkError::new(ErrorKind::Cancelled)); }
            else if result.is_ok() && network.generation() != generation { result = Err(NetworkError::new(ErrorKind::ServiceChanged)); }
            if let Err(error) = &mut result { error.request_id = Some(id.to_string()); if sent.load(Ordering::SeqCst) && error.outcome != Outcome::Rejected { error.outcome = Outcome::Unknown; } }
            // 回调允许再次调用 Network；不在持有实例锁时执行用户代码。
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match result { Ok(response) => on_success(response), Err(error) => on_error(error) }));
            network.inner.state.lock().unwrap().requests.remove(&id);
            network.inner.done.notify_waiters();
        });
        RequestHandle { cancel }
    }
    pub async fn shutdown(&self) {
        {
            let mut state = self.inner.state.lock().unwrap();
            state.closed = true;
            // 主动唤醒每个请求，让调用者收到取消回调，而非等待连接超时。
            for cancel in state.requests.values() {
                cancel.send_replace(true);
            }
            state.generation += 1;
            if let Some(service) = state.service.take() {
                state.retired.push(service);
            }
            self.inner.changed.send_replace(state.generation);
        }
        self.retire().await;
        loop {
            let done = self.inner.done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if self.inner.state.lock().unwrap().requests.is_empty() {
                break;
            }
            done.await;
        }
    }

    pub async fn close(&self) {
        self.shutdown().await;
    }
}
