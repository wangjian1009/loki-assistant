use http::{Method, StatusCode};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, SecretKey,
    endpoint::{Connection, RecvStream, presets},
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

pub use crate::error::*;
use crate::pairing::Invitation;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(try_from = "CredentialData")]
pub struct BindingCredential {
    token: String,
    bound_at: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialData {
    token: String,
    bound_at: u64,
}
impl TryFrom<CredentialData> for BindingCredential {
    type Error = &'static str;
    fn try_from(value: CredentialData) -> Result<Self, Self::Error> {
        if value.token.len() != 64
            || !value
                .token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || value.bound_at == 0
        {
            return Err("绑定凭据格式无效");
        }
        Ok(Self {
            token: value.token,
            bound_at: value.bound_at,
        })
    }
}
impl std::fmt::Debug for BindingCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingCredential")
            .field("token", &"[REDACTED]")
            .field("bound_at", &self.bound_at)
            .finish()
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", try_from = "BindingData")]
pub enum BindingState {
    #[default]
    Unbound,
    Binding,
    Failed {
        error: ResponsError,
    },
    Bound {
        credential: BindingCredential,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    status: String,
    credential: Option<BindingCredential>,
    error: Option<ResponsError>,
}
impl TryFrom<BindingData> for BindingState {
    type Error = &'static str;
    fn try_from(data: BindingData) -> Result<Self, Self::Error> {
        match (data.status.as_str(), data.credential, data.error) {
            ("unbound", None, None) => Ok(Self::Unbound),
            ("binding", None, None) => Ok(Self::Binding),
            ("failed", None, Some(error)) => Ok(Self::Failed { error }),
            ("bound", Some(credential), None) => Ok(Self::Bound { credential }),
            _ => Err("绑定状态与协议数据不一致"),
        }
    }
}

/// 仅自动绑定使用，成功或失败后均丢弃一次性邀请；不实现 Debug。
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutoBinding {
    pub invitation: Invitation,
    pub device_name: String,
}
#[derive(Clone, Serialize)]
pub struct ServiceState {
    pub endpoint_id: String,
    pub relay_url: String,
    pub relay_token: String,
    pub binding: BindingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_binding: Option<AutoBinding>,
    pub generation: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Binding {
    endpoint_id: String,
    credential: BindingCredential,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryParameter {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug)]
pub struct ServiceRequest<B = Value> {
    pub method: Method,
    pub path: String,
    pub query: Vec<QueryParameter>,
    pub body: Option<B>,
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

#[derive(Clone, Copy)]
pub struct SseRequestOptions {
    pub open_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for SseRequestOptions {
    fn default() -> Self {
        Self {
            open_timeout: RequestOptions::default().timeout,
            idle_timeout: Duration::from_secs(40),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SseEvent<T> {
    pub event: String,
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug)]
pub enum SseUpdate<T> {
    Opened(ServiceResponse<()>),
    Event(T),
}

pub trait SseEventType: DeserializeOwned {
    fn supported(&self) -> bool {
        true
    }
}

impl<T: DeserializeOwned> SseEventType for SseEvent<T> {}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub enum ApiVersion {
    #[serde(rename = "1")]
    V1,
}

#[derive(Serialize)]
struct WireRequest<'a, B> {
    api_version: ApiVersion,
    request_id: Uuid,
    binding_token: &'a str,
    method: &'a str,
    path: &'a str,
    query: &'a [QueryParameter],
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a B>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mode: Option<ResponseMode>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResponseMode {
    Sse,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(rename = "api_version")]
    _api_version: ApiVersion,
    request_id: Uuid,
    status: u16,
    body: Value,
}

#[derive(Deserialize)]
struct SseResponseHead {
    #[serde(rename = "api_version")]
    _api_version: ApiVersion,
    request_id: Uuid,
    status: u16,
    content_type: StreamContentType,
    body: Option<Value>,
}

#[derive(Deserialize)]
enum StreamContentType {
    #[serde(rename = "text/event-stream")]
    Events,
    #[serde(rename = "application/json")]
    Json,
}

impl SseResponseHead {
    fn status(self, id: Uuid) -> Result<StatusCode, ResponsError> {
        if self.request_id != id {
            return Err(NetworkReason::InvalidResponse.into());
        }
        let status = StatusCode::from_u16(self.status)
            .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
        match self.content_type {
            StreamContentType::Events if status.is_success() && self.body.is_none() => Ok(status),
            StreamContentType::Json if status.is_client_error() || status.is_server_error() => {
                Err(ResponsError::api(
                    &self.body.ok_or(NetworkReason::InvalidResponse)?,
                    self.status,
                )?)
            }
            _ => Err(NetworkReason::InvalidResponse.into()),
        }
    }
}

async fn read_frame(
    recv: &mut RecvStream,
    limit: usize,
    idle: Duration,
) -> Result<Option<Vec<u8>>, ResponsError> {
    async fn fill(
        recv: &mut RecvStream,
        mut buffer: &mut [u8],
        idle: Duration,
        mut at_boundary: bool,
    ) -> Result<bool, ResponsError> {
        while !buffer.is_empty() {
            match tokio::time::timeout(idle, recv.read(buffer)).await {
                Ok(Ok(Some(n))) => {
                    at_boundary = false;
                    buffer = &mut buffer[n..];
                }
                Ok(Ok(None)) if at_boundary => return Ok(false),
                Ok(_) => return Err(NetworkReason::ReceiveFailed.into()),
                Err(_) => return Err(NetworkReason::RequestTimeout.into()),
            }
        }
        Ok(true)
    }

    let mut length = [0; 4];
    if !fill(recv, &mut length, idle, true).await? {
        return Ok(None);
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > limit {
        return Err(NetworkReason::ResponseTooLarge.into());
    }
    let mut bytes = vec![0; length];
    fill(recv, &mut bytes, idle, false).await?;
    Ok(Some(bytes))
}

/// 应用设置的唯一电脑服务；凭据不进入状态快照或错误。
pub struct Service {
    identity: SecretKey,
    id: EndpointId,
    relay_url: String,
    relay_token: String,
    endpoint: AsyncMutex<Option<Endpoint>>,
    connection: AsyncMutex<Option<Connection>>,
    closed: AtomicBool,
    binding: Mutex<BindingState>,
    auto_binding: Mutex<Option<AutoBinding>>,
}
impl Service {
    pub fn new(
        identity: SecretKey,
        id: EndpointId,
        relay_url: String,
        relay_token: String,
    ) -> Result<Self, ResponsError> {
        if identity.public() == id || !crate::pairing::valid_relay(&relay_url, &relay_token) {
            return Err(ResponsError::from(InvalidRequestReason::InvalidService));
        }
        Ok(Self {
            identity,
            id,
            relay_url,
            relay_token,
            endpoint: AsyncMutex::new(None),
            connection: AsyncMutex::new(None),
            closed: AtomicBool::new(false),
            binding: Mutex::new(BindingState::Unbound),
            auto_binding: Mutex::new(None),
        })
    }
    pub fn with_binding(self, binding: BindingState) -> Self {
        *self.binding.lock().unwrap() = binding;
        self
    }
    pub fn with_auto_binding(self, options: AutoBinding) -> Self {
        // 已有票据或最终失败结果不因残留邀请数据而重新握手。
        if matches!(
            self.binding(),
            BindingState::Bound { .. } | BindingState::Failed { .. }
        ) {
            return self;
        }
        *self.auto_binding.lock().unwrap() = Some(options);
        *self.binding.lock().unwrap() = BindingState::Unbound;
        self
    }
    pub fn binding(&self) -> BindingState {
        self.binding.lock().unwrap().clone()
    }
    pub fn endpoint_id(&self) -> EndpointId {
        self.id
    }
    fn same_binding(&self, other: &Self) -> bool {
        self.id == other.id
            && self.identity.public() == other.identity.public()
            && self.relay_url == other.relay_url
            && self.relay_token == other.relay_token
            && (self.binding() == other.binding()
                || matches!(
                    (self.binding(), other.binding()),
                    (BindingState::Binding, BindingState::Unbound)
                ))
            && *self.auto_binding.lock().unwrap() == *other.auto_binding.lock().unwrap()
    }
    async fn endpoint(&self) -> Result<Endpoint, ResponsError> {
        let mut guard = self.endpoint.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(ResponsError::from(CancelledReason::ServiceChanged));
        }
        if let Some(endpoint) = guard.as_ref() {
            return Ok(endpoint.clone());
        }
        let relay: iroh::RelayUrl = self
            .relay_url
            .parse()
            .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidService))?;
        let relays: RelayMap = [relay].into_iter().collect();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(self.identity.clone())
            .clear_ip_transports()
            .relay_mode(RelayMode::Custom(
                relays.with_auth_token(self.relay_token.clone()),
            ))
            .bind()
            .await
            .map_err(|_| ResponsError::from(NetworkReason::ConnectFailed))?;
        *guard = Some(endpoint.clone());
        Ok(endpoint)
    }
    async fn connect(&self, alpn: &[u8]) -> Result<Connection, ResponsError> {
        let endpoint = self.endpoint().await?;
        let remote = EndpointAddr::new(self.id).with_relay_url(
            self.relay_url
                .parse()
                .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidService))?,
        );
        tokio::time::timeout(Duration::from_secs(10), endpoint.connect(remote, alpn))
            .await
            .map_err(|_| ResponsError::from(NetworkReason::ConnectTimeout))?
            .map_err(|_| ResponsError::from(NetworkReason::ConnectFailed))
    }
    async fn connection(&self) -> Result<Connection, ResponsError> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref().filter(|c| c.close_reason().is_none()) {
            return Ok(connection.clone());
        }
        let connection = self.connect(SERVICES_ALPN).await?;
        *guard = Some(connection.clone());
        Ok(connection)
    }
    async fn send_request(
        &self,
        request: &ServiceRequest<impl Serialize>,
        id: Uuid,
        sent: &AtomicBool,
        response_mode: Option<ResponseMode>,
    ) -> Result<RecvStream, ResponsError> {
        if !request.path.starts_with('/')
            || request.path.starts_with("//")
            || request.path.contains(['?', '#', '\\'])
            || request.path.chars().any(char::is_control)
        {
            return Err(InvalidRequestReason::InvalidPath.into());
        }
        let binding = self.binding();
        let BindingState::Bound { credential } = binding else {
            return Err(NotBoundReason::BindingRequired.into());
        };
        let bytes = serde_json::to_vec(&WireRequest {
            api_version: ApiVersion::V1,
            request_id: id,
            binding_token: &credential.token,
            method: request.method.as_str(),
            path: &request.path,
            query: &request.query,
            body: request.body.as_ref(),
            response_mode,
        })
        .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidBody))?;
        if bytes.len() > 1_048_576 {
            return Err(InvalidRequestReason::RequestTooLarge.into());
        }
        let connection = self.connection().await?;
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        sent.store(true, Ordering::SeqCst);
        send.write_all(&bytes)
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        send.finish()
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        Ok(recv)
    }

    async fn exchange(
        &self,
        request: &ServiceRequest<impl Serialize>,
        id: Uuid,
        sent: &AtomicBool,
    ) -> Result<(StatusCode, Value), ResponsError> {
        let mut recv = self.send_request(request, id, sent, None).await?;
        let bytes = recv
            .read_to_end(4_194_304)
            .await
            .map_err(|error| match error {
                iroh::endpoint::ReadToEndError::TooLong => {
                    ResponsError::from(NetworkReason::ResponseTooLarge)
                }
                _ => ResponsError::from(NetworkReason::ReceiveFailed),
            })?;
        let reply: WireResponse = serde_json::from_slice(&bytes)
            .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
        if reply.request_id != id {
            return Err(NetworkReason::InvalidResponse.into());
        }
        let status = StatusCode::from_u16(reply.status)
            .ok()
            .filter(|s| s.is_success() || s.is_client_error() || s.is_server_error())
            .ok_or_else(|| ResponsError::from(NetworkReason::InvalidResponse))?;
        if !status.is_success() {
            return Err(ResponsError::api(&reply.body, status.as_u16())?);
        }
        if status == StatusCode::NO_CONTENT && !reply.body.is_null() {
            return Err(NetworkReason::InvalidResponse.into());
        }
        Ok((status, reply.body))
    }
    async fn pair(
        &self,
        invitation: &Invitation,
        device_name: &str,
        sent: &AtomicBool,
    ) -> Result<Binding, ResponsError> {
        if invitation.endpoint_id != self.id.to_string()
            || invitation.relay_url != self.relay_url
            || invitation.relay_token != self.relay_token
        {
            return Err(InvalidRequestReason::InvitationTargetMismatch.into());
        }
        let connection = self.connect(PAIR_ALPN).await?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        let request = json!({"api_version":"1","type":"pairing.bind","pairing_id":invitation.pairing_id,"pairing_token":invitation.pairing_token,"device_name":device_name,"device_type":"mobile"});
        sent.store(true, Ordering::SeqCst);
        send.write_all(&serde_json::to_vec(&request).unwrap())
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        send.finish()
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        let result = async {
            let bytes = recv
                .read_to_end(16_384)
                .await
                .map_err(|_| ResponsError::from(NetworkReason::ReceiveFailed))?;
            let reply: Value = serde_json::from_slice(&bytes)
                .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
            if let Some(status) = reply["status"].as_u64() {
                let status = u16::try_from(status)
                    .ok()
                    .filter(|s| (400..600).contains(s))
                    .ok_or_else(|| ResponsError::from(NetworkReason::InvalidResponse))?;
                return Err(ResponsError::api(&reply, status)?);
            }
            if reply["api_version"] != "1"
                || reply["type"] != "pairing.bound"
                || reply["endpoint_id"] != self.id.to_string()
            {
                return Err(NetworkReason::InvalidResponse.into());
            }
            let credential: BindingCredential = serde_json::from_value(
                json!({"token":reply["binding_token"],"bound_at":reply["bound_at"]}),
            )
            .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
            Ok(Binding {
                endpoint_id: self.id.to_string(),
                credential,
            })
        }
        .await;
        connection.close(0u32.into(), b"finished");
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
    pub fn set_service(&self, service: Service) -> Result<(), ResponsError> {
        let mut state = self.inner.state.lock().unwrap();
        if state.closed {
            return Err(CancelledReason::NetworkClosed.into());
        }
        if state
            .service
            .as_ref()
            .is_some_and(|old| old.same_binding(&service))
        {
            return Ok(());
        }
        let service = Arc::new(service);
        let auto_bind = matches!(
            service.binding(),
            BindingState::Unbound | BindingState::Binding
        );
        if auto_bind {
            *service.binding.lock().unwrap() = BindingState::Binding;
        }
        if let Some(old) = state.service.replace(service.clone()) {
            state.retired.push(old);
        }
        state.generation += 1;
        self.inner.changed.send_replace(state.generation);
        if auto_bind {
            // 在释放状态锁前登记任务，shutdown 必须等待这次绑定收尾。
            let id = Uuid::new_v4();
            let (cancel, cancelled) = watch::channel(false);
            state.requests.insert(id, cancel);
            let generation = state.generation;
            let changed = self.inner.changed.subscribe();
            let network = self.clone();
            self.inner.runtime.spawn(async move {
                network
                    .auto_bind(service, id, generation, cancelled, changed)
                    .await;
            });
        }
        drop(state);
        self.schedule_retirement();
        Ok(())
    }

    async fn auto_bind(
        &self,
        service: Arc<Service>,
        id: Uuid,
        generation: u64,
        mut cancelled: watch::Receiver<bool>,
        mut changed: watch::Receiver<u64>,
    ) {
        let sent = AtomicBool::new(false);
        let operation = async {
            self.retire().await;
            let options = service
                .auto_binding
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| ResponsError::from(InvalidRequestReason::InvalidInvitation))?;
            let invitation =
                Invitation::parse(&serde_json::to_string(&options.invitation).unwrap())?;
            let name = options.device_name.trim();
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(InvalidRequestReason::InvalidDeviceName.into());
            }
            service.pair(&invitation, name, &sent).await
        };
        let mut result = tokio::select! {
            biased;
            _ = async { if !*cancelled.borrow() { let _ = cancelled.changed().await; } } => Err(CancelledReason::NetworkClosed.into()),
            _ = async { if *changed.borrow() == generation { let _ = changed.changed().await; } } => Err(CancelledReason::ServiceChanged.into()),
            result = tokio::time::timeout(Duration::from_secs(20), operation) => result.unwrap_or_else(|_| Err(NetworkReason::RequestTimeout.into())),
        };
        if let Err(error) = &mut result {
            let outcome = if sent.load(Ordering::SeqCst) && error.outcome() != Outcome::Rejected {
                Outcome::Unknown
            } else {
                error.outcome()
            };
            error.set_context(Some(id.to_string()), outcome);
        }
        {
            let mut state = self.inner.state.lock().unwrap();
            // 旧目标的成功、失败或取消都不能覆盖当前 Service。
            if state.generation == generation && !state.closed {
                *service.binding.lock().unwrap() = match result {
                    Ok(binding) => BindingState::Bound {
                        credential: binding.credential,
                    },
                    Err(error) => BindingState::Failed { error },
                };
                service.auto_binding.lock().unwrap().take();
            }
            state.requests.remove(&id);
        }
        self.inner.done.notify_waiters();
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
    pub fn service(&self) -> Option<ServiceState> {
        let state = self.inner.state.lock().unwrap();
        state.service.as_ref().map(|service| ServiceState {
            endpoint_id: service.id.to_string(),
            relay_url: service.relay_url.clone(),
            relay_token: service.relay_token.clone(),
            binding: service.binding(),
            auto_binding: service.auto_binding.lock().unwrap().clone(),
            generation: state.generation,
        })
    }

    pub(crate) fn target_changes(&self) -> watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    pub(crate) fn with_target<T>(
        &self,
        read: impl FnOnce(u64, Option<(EndpointId, BindingState)>, bool) -> T,
    ) -> T {
        let state = self.inner.state.lock().unwrap();
        let target = state
            .service
            .as_ref()
            .map(|service| (service.id, service.binding()));
        read(state.generation, target, state.closed)
    }

    pub fn request<T, OnSuccess, OnError>(
        &self,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
        options: RequestOptions,
        on_success: OnSuccess,
        on_error: OnError,
    ) -> RequestHandle
    where
        T: DeserializeOwned + Send + 'static,
        OnSuccess: FnOnce(ServiceResponse<T>) + Send + 'static,
        OnError: FnOnce(ResponsError) + Send + 'static,
    {
        self.request_for(None, request, options, on_success, on_error)
    }

    pub(crate) fn request_for<T, OnSuccess, OnError>(
        &self,
        expected: Option<u64>,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
        options: RequestOptions,
        on_success: OnSuccess,
        on_error: OnError,
    ) -> RequestHandle
    where
        T: DeserializeOwned + Send + 'static,
        OnSuccess: FnOnce(ServiceResponse<T>) + Send + 'static,
        OnError: FnOnce(ResponsError) + Send + 'static,
    {
        self.execute(
            expected,
            move |network, service, id, _, sent| async move {
                if !(Duration::from_secs(1)..=Duration::from_secs(30)).contains(&options.timeout) {
                    return Err(InvalidRequestReason::InvalidTimeout.into());
                }
                tokio::time::timeout(options.timeout, async {
                    network.retire().await;
                    let (status, value) = service.exchange(&request, id, &sent).await?;
                    let data = serde_json::from_value(value)
                        .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
                    Ok((status, data))
                })
                .await
                .map_err(|_| ResponsError::from(NetworkReason::RequestTimeout))?
            },
            move |result| match result {
                Ok(response) => on_success(response),
                Err(error) => on_error(error),
            },
        )
    }
    pub fn request_sse<T: SseEventType + Send + 'static>(
        &self,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
        options: SseRequestOptions,
        mut on_update: impl FnMut(SseUpdate<T>) + Send + 'static,
        on_end: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> RequestHandle {
        self.request_sse_for(
            None,
            request,
            options,
            move |update| on_update(update),
            on_end,
        )
    }

    pub(crate) fn request_sse_for<T: SseEventType + Send + 'static>(
        &self,
        expected: Option<u64>,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
        options: SseRequestOptions,
        mut on_update: impl FnMut(SseUpdate<T>) + Send + 'static,
        on_end: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> RequestHandle {
        self.execute(
            expected,
            move |network, service, id, generation, sent| async move {
                if !(Duration::from_secs(1)..=Duration::from_secs(30))
                    .contains(&options.open_timeout)
                    || options.idle_timeout.is_zero()
                {
                    return Err(InvalidRequestReason::InvalidTimeout.into());
                }
                let (mut recv, status) = tokio::time::timeout(options.open_timeout, async {
                    network.retire().await;
                    let mut recv = service
                        .send_request(&request, id, &sent, Some(ResponseMode::Sse))
                        .await?;
                    let head = read_frame(&mut recv, 16_384, options.open_timeout)
                        .await?
                        .ok_or(NetworkReason::InvalidResponse)?;
                    let head: SseResponseHead = serde_json::from_slice(&head)
                        .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
                    Ok::<_, ResponsError>((recv, head.status(id)?))
                })
                .await
                .map_err(|_| ResponsError::from(NetworkReason::RequestTimeout))??;
                let mut update = |value| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_update(value)))
                        .map_err(|_| ResponsError::from(CancelledReason::UserRequested))
                };
                update(SseUpdate::Opened(ServiceResponse {
                    request_id: id,
                    status,
                    generation,
                    data: (),
                }))?;
                while let Some(frame) =
                    read_frame(&mut recv, 1_048_576, options.idle_timeout).await?
                {
                    if !frame.is_empty() {
                        let event: T = serde_json::from_slice(&frame)
                            .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
                        if event.supported() {
                            update(SseUpdate::Event(event))?;
                        }
                    }
                    // 缓冲中已有数据时也让目标切换和取消信号有机会被处理。
                    tokio::task::yield_now().await;
                }
                Ok((status, ()))
            },
            move |result| on_end(result.map(|_| ())),
        )
    }

    fn execute<T, Operation, Future, OnEnd>(
        &self,
        expected: Option<u64>,
        operation: Operation,
        on_end: OnEnd,
    ) -> RequestHandle
    where
        T: Send + 'static,
        Operation:
            FnOnce(Self, Arc<Service>, Uuid, u64, Arc<AtomicBool>) -> Future + Send + 'static,
        Future: std::future::Future<Output = Result<(StatusCode, T), ResponsError>> + Send,
        OnEnd: FnOnce(Result<ServiceResponse<T>, ResponsError>) + Send + 'static,
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
        if closed || expected.is_some_and(|expected| expected != generation) {
            self.inner.state.lock().unwrap().requests.remove(&id);
            let mut error = ResponsError::from(if closed {
                CancelledReason::NetworkClosed
            } else {
                CancelledReason::ServiceChanged
            });
            error.set_context(Some(id.to_string()), Outcome::NotSent);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_end(Err(error))));
            self.inner.done.notify_waiters();
            return RequestHandle { cancel };
        }
        let network = self.clone();
        self.inner.runtime.spawn(async move {
            let sent = Arc::new(AtomicBool::new(false));
            let mut result = if *cancelled.borrow() {
                Err(CancelledReason::UserRequested.into())
            } else if let Some(service) = service.as_ref() {
                if !matches!(service.binding(), BindingState::Bound { .. }) {
                    Err(NotBoundReason::BindingRequired.into())
                } else {
                    tokio::select! {
                        biased;
                        _ = async {
                            if !*cancelled.borrow() { let _ = cancelled.changed().await; }
                        } => Err(CancelledReason::UserRequested.into()),
                        _ = async {
                            if *changed.borrow() == generation { let _ = changed.changed().await; }
                        } => {
                            let reason = if network.inner.state.lock().unwrap().closed {
                                CancelledReason::NetworkClosed
                            } else { CancelledReason::ServiceChanged };
                            Err(reason.into())
                        },
                        result = operation(network.clone(), service.clone(), id, generation, sent.clone()) => result,
                    }
                }
            } else {
                Err(NotConfiguredReason::ServiceNotSet.into())
            };
            if *cancelled.borrow() {
                result = Err(CancelledReason::UserRequested.into());
            }
            {
                let state = network.inner.state.lock().unwrap();
                if state.generation != generation {
                    let reason = if state.closed {
                        CancelledReason::NetworkClosed
                    } else {
                        CancelledReason::ServiceChanged
                    };
                    result = Err(reason.into());
                } else if let Some(service) = service {
                    if let Err(error @ ResponsError::NotBound(_)) = &result {
                        // 正在绑定时业务请求只是未就绪，不中断或覆盖自动绑定。
                        if matches!(service.binding(), BindingState::Bound { .. }) {
                            *service.binding.lock().unwrap() = BindingState::Failed {
                                error: error.clone(),
                            };
                        }
                    }
                }
            }
            if let Err(error) = &mut result {
                let outcome = if sent.load(Ordering::SeqCst) && error.outcome() != Outcome::Rejected
                {
                    Outcome::Unknown
                } else {
                    error.outcome()
                };
                error.set_context(Some(id.to_string()), outcome);
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                on_end(result.map(|(status, data)| ServiceResponse {
                    request_id: id,
                    status,
                    data,
                    generation,
                }))
            }));
            network.inner.state.lock().unwrap().requests.remove(&id);
            network.inner.done.notify_waiters();
        });
        RequestHandle { cancel }
    }
    /// 同步发出关闭信号，避免原生工作线程被请求占满时无法启动取消。
    pub fn begin_shutdown(&self) {
        let mut state = self.inner.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        for cancel in state.requests.values() {
            cancel.send_replace(true);
        }
        state.generation += 1;
        if let Some(service) = state.service.take() {
            state.retired.push(service);
        }
        self.inner.changed.send_replace(state.generation);
    }
    pub async fn shutdown(&self) {
        self.begin_shutdown();
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
