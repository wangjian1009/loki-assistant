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
    Bound {
        credential: BindingCredential,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    status: String,
    credential: Option<BindingCredential>,
}
impl TryFrom<BindingData> for BindingState {
    type Error = &'static str;
    fn try_from(data: BindingData) -> Result<Self, Self::Error> {
        match (data.status.as_str(), data.credential) {
            ("unbound", None) => Ok(Self::Unbound),
            ("bound", Some(credential)) => Ok(Self::Bound { credential }),
            _ => Err("绑定状态与协议凭据不一致"),
        }
    }
}
#[derive(Clone, Serialize)]
pub struct ServiceState {
    pub endpoint_id: String,
    pub relay_url: String,
    pub relay_token: String,
    pub binding: BindingState,
    pub generation: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Binding {
    pub endpoint_id: String,
    pub credential: BindingCredential,
}

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
        })
    }
    pub fn with_binding(self, binding: BindingState) -> Self {
        *self.binding.lock().unwrap() = binding;
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
            && self.binding() == other.binding()
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
    async fn exchange(
        &self,
        request: &ServiceRequest,
        id: Uuid,
        sent: &AtomicBool,
    ) -> Result<(StatusCode, Value), ResponsError> {
        let binding = self.binding();
        let BindingState::Bound { credential } = binding else {
            return Err(NotBoundReason::BindingRequired.into());
        };
        let mut body = json!({"binding_token":credential.token,"api_version":"1","request_id":id.to_string(),"method":request.method.as_str(),"path":request.path,"query":request.query});
        if let Some(value) = &request.body {
            body["body"] = value.clone();
        }
        let bytes = serde_json::to_vec(&body).unwrap();
        if bytes.len() > 1_048_576 {
            return Err(InvalidRequestReason::RequestTooLarge.into());
        }
        let connection = self.connection().await?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        sent.store(true, Ordering::SeqCst);
        send.write_all(&bytes)
            .await
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        send.finish()
            .map_err(|_| ResponsError::from(NetworkReason::SendFailed))?;
        let bytes = recv
            .read_to_end(4_194_304)
            .await
            .map_err(|error| match error {
                iroh::endpoint::ReadToEndError::TooLong => {
                    ResponsError::from(NetworkReason::ResponseTooLarge)
                }
                _ => ResponsError::from(NetworkReason::ReceiveFailed),
            })?;
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
        if reply["api_version"] != "1"
            || reply["request_id"] != id.to_string()
            || reply.get("body").is_none()
        {
            return Err(NetworkReason::InvalidResponse.into());
        }
        let status = reply["status"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok())
            .and_then(|s| StatusCode::from_u16(s).ok())
            .filter(|s| s.is_success() || s.is_client_error() || s.is_server_error())
            .ok_or_else(|| ResponsError::from(NetworkReason::InvalidResponse))?;
        if !status.is_success() {
            return Err(ResponsError::api(&reply["body"], status.as_u16())?);
        }
        if status == StatusCode::NO_CONTENT && !reply["body"].is_null() {
            return Err(NetworkReason::InvalidResponse.into());
        }
        Ok((status, reply["body"].clone()))
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
            return Err(ResponsError::from(CancelledReason::NetworkClosed));
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
            generation: state.generation,
        })
    }
    pub fn bind<OnSuccess, OnError>(
        &self,
        payload: &str,
        device_name: &str,
        on_success: OnSuccess,
        on_error: OnError,
    ) -> RequestHandle
    where
        OnSuccess: FnOnce(ServiceResponse<Binding>) + Send + 'static,
        OnError: FnOnce(ResponsError) + Send + 'static,
    {
        let invitation = Invitation::parse(payload);
        let device_name = device_name.to_owned();
        self.execute(
            true,
            RequestOptions {
                timeout: Duration::from_secs(20),
            },
            move |service, _, sent| async move {
                if device_name.trim().is_empty()
                    || device_name.len() > 128
                    || device_name.chars().any(char::is_control)
                {
                    return Err(InvalidRequestReason::InvalidDeviceName.into());
                }
                let binding = service
                    .pair(&invitation?, device_name.trim(), &sent)
                    .await?;
                let state = BindingState::Bound {
                    credential: binding.credential.clone(),
                };
                Ok((StatusCode::OK, binding, Some(state)))
            },
            on_success,
            on_error,
        )
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
        OnError: FnOnce(ResponsError) + Send + 'static,
    {
        self.execute(
            false,
            options,
            move |service, id, sent| async move {
                if !request.path.starts_with('/')
                    || request.path.starts_with("//")
                    || request.path.contains(['?', '#', '\\'])
                    || request.path.chars().any(char::is_control)
                {
                    return Err(InvalidRequestReason::InvalidPath.into());
                }
                let (status, value) = service.exchange(&request, id, &sent).await?;
                let data = serde_json::from_value(value)
                    .map_err(|_| ResponsError::from(NetworkReason::InvalidResponse))?;
                Ok((status, data, None))
            },
            on_success,
            on_error,
        )
    }
    fn execute<T, Operation, Future, OnSuccess, OnError>(
        &self,
        allow_unbound: bool,
        options: RequestOptions,
        operation: Operation,
        on_success: OnSuccess,
        on_error: OnError,
    ) -> RequestHandle
    where
        T: Send + 'static,
        Operation: FnOnce(Arc<Service>, Uuid, Arc<AtomicBool>) -> Future + Send + 'static,
        Future: std::future::Future<
                Output = Result<(StatusCode, T, Option<BindingState>), ResponsError>,
            > + Send,
        OnSuccess: FnOnce(ServiceResponse<T>) + Send + 'static,
        OnError: FnOnce(ResponsError) + Send + 'static,
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
        if closed {
            self.inner.state.lock().unwrap().requests.remove(&id);
            let mut error = ResponsError::from(CancelledReason::NetworkClosed);
            error.set_context(Some(id.to_string()), Outcome::NotSent);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_error(error)));
            self.inner.done.notify_waiters();
            return RequestHandle { cancel };
        }
        let network = self.clone();
        self.inner.runtime.spawn(async move {
            let sent = Arc::new(AtomicBool::new(false));
            let mut result = if *cancelled.borrow() {
                Err(CancelledReason::UserRequested.into())
            } else if let Some(service) = service.as_ref() {
                if !allow_unbound && matches!(service.binding(), BindingState::Unbound) {
                    Err(NotBoundReason::BindingRequired.into())
                } else if !(Duration::from_secs(1)..=Duration::from_secs(30))
                    .contains(&options.timeout)
                {
                    Err(InvalidRequestReason::InvalidTimeout.into())
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
                        result = tokio::time::timeout(options.timeout, async {
                            network.retire().await;
                            operation(service.clone(), id, sent.clone()).await
                        }) => result.unwrap_or_else(|_| Err(NetworkReason::RequestTimeout.into())),
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
                    if let Ok((_, _, Some(binding))) = &result {
                        *service.binding.lock().unwrap() = binding.clone();
                    } else if matches!(&result, Err(ResponsError::NotBound(_))) {
                        *service.binding.lock().unwrap() = BindingState::Unbound;
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
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match result {
                Ok((status, data, _)) => on_success(ServiceResponse {
                    request_id: id,
                    status,
                    data,
                    generation,
                }),
                Err(error) => on_error(error),
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
