use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, broadcast, oneshot, watch},
    task::JoinHandle,
};

use crate::network::{
    ApiVersion, BindingState, CancelledReason, InvalidRequestReason, Network, NetworkReason,
    NotBoundReason, NotConfiguredReason, RequestHandle, RequestOptions, ResponsError,
    ServiceRequest, ServiceResponse, SseEventType, SseRequestOptions, SseUpdate,
};

type Values = BTreeMap<String, Value>;
type StatusValues = BTreeMap<String, Values>;

#[derive(Clone, Default, PartialEq, Serialize)]
pub struct ServiceSnapshot {
    pub generation: u64,
    pub endpoint_id: Option<String>,
    pub checked_at: u64,
    pub error: Option<ResponsError>,
    pub status: Option<StatusValues>,
    local: StatusValues,
    #[serde(skip)]
    status_revision: u64,
}

#[derive(Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum DeviceRefreshPhase {
    Registering,
    Reconnecting,
    Querying,
    Succeeded,
    Failed { error: ResponsError },
}

#[derive(Clone, Serialize)]
pub struct DeviceRefreshEvent {
    pub endpoint_id: String,
    pub generation: u64,
    #[serde(flatten)]
    pub phase: DeviceRefreshPhase,
}

impl ServiceSnapshot {
    pub fn status<T: DeserializeOwned>(&self, module: &str) -> Option<T> {
        let values = self.status.as_ref()?.get(module)?;
        serde_json::from_value(serde_json::to_value(values).ok()?).ok()
    }

    pub fn local<T: DeserializeOwned>(&self, module: &str, key: &str) -> Option<T> {
        serde_json::from_value(self.local.get(module)?.get(key)?.clone()).ok()
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct ComputerStatus {
    pub name: String,
    pub device_type: ComputerDeviceType,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerDeviceType {
    Computer,
}

impl ComputerStatus {
    pub fn from_snapshot(snapshot: &ServiceSnapshot) -> Option<Self> {
        let status: Self = snapshot.status("service")?;
        (!status.name.trim().is_empty()
            && status.name.len() <= 128
            && !status.name.chars().any(char::is_control))
        .then_some(status)
    }
}

#[derive(Deserialize)]
struct StatusResponse {
    #[serde(rename = "api_version")]
    _api_version: ApiVersion,
    status: StatusValues,
}

impl StatusResponse {
    fn into_status(self) -> Result<StatusValues, ResponsError> {
        let snapshot = ServiceSnapshot {
            status: Some(self.status),
            ..Default::default()
        };
        ComputerStatus::from_snapshot(&snapshot)
            .and(snapshot.status)
            .ok_or_else(|| NetworkReason::InvalidResponse.into())
    }
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StatusEvent {
    Status {
        data: StatusResponse,
    },
    #[serde(other)]
    Unsupported,
}

impl SseEventType for StatusEvent {}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub apis: Vec<ApiMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiMetadata {
    pub id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub access: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<ApiParameter>,
    pub request: Option<ApiDataMetadata>,
    pub response: Option<ApiDataMetadata>,
    #[serde(default)]
    pub supports_sse: bool,
    #[serde(default)]
    pub sse_events: Vec<SseEventMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiParameter {
    pub location: String,
    #[serde(flatten)]
    pub field: ApiField,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiDataMetadata {
    pub name: String,
    pub fields: Vec<ApiField>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiField {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub description: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct SseEventMetadata {
    pub event: String,
    pub data_schema: Value,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ConfigMetadata {
    pub modules: Vec<ConfigModuleMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ConfigModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub items: Vec<ConfigItemMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ConfigItemMetadata {
    pub key: String,
    pub name: String,
    pub description: String,
    pub data_type: ConfigDataType,
    pub required: bool,
    pub default: Option<Value>,
    #[serde(flatten)]
    pub attributes: Values,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ConfigDataType {
    Basic(ConfigBasicType),
    Password { password: PasswordConfigType },
    Reference { r#ref: String },
    Unknown(Value),
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PasswordConfigType {
    pub path: String,
    pub key: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigBasicType {
    String,
    Key,
    Integer,
    Number,
    Boolean,
    Dictionary,
    Structure,
    PasswordReference,
    List,
    FilePath,
    Executable,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusMetadata {
    pub api_version: ApiVersion,
    pub modules: Vec<StatusModuleMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusModuleMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub items: Vec<StatusItemMetadata>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusItemMetadata {
    pub key: String,
    pub name: String,
    pub description: String,
    pub remote_access: bool,
    pub schema: StatusSchema,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "data_type", rename_all = "snake_case")]
pub enum StatusSchema {
    String {
        #[serde(default)]
        choices: Vec<StatusChoice>,
    },
    Integer,
    Number,
    Boolean,
    Null,
    Object {
        fields: Vec<StatusField>,
    },
    Array {
        items: Box<StatusSchema>,
    },
    Any,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusField {
    pub key: String,
    pub name: String,
    pub description: String,
    pub required: bool,
    pub nullable: bool,
    pub schema: StatusSchema,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusChoice {
    pub value: Value,
    pub label: String,
    pub description: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ModuleConfig {
    pub module_id: String,
    pub values: BTreeMap<String, ConfigValue>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ConfigValue {
    pub value: Value,
    pub source: ConfigSource,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    User,
    Default,
    #[serde(other)]
    Unknown,
}

#[derive(Default)]
struct Cache {
    generation: u64,
    apis: BTreeMap<String, ApiModuleMetadata>,
    config_meta: Option<ConfigMetadata>,
    status_meta: Option<StatusMetadata>,
    config: BTreeMap<String, ModuleConfig>,
}

pub struct StatusSubscription {
    task: JoinHandle<()>,
}

impl StatusSubscription {
    pub fn cancel(&self) {
        self.task.abort();
    }
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct ServiceProxy {
    network: Arc<Network>,
    runtime: tokio::runtime::Handle,
    state: watch::Sender<ServiceSnapshot>,
    requested: Arc<Notify>,
    refresh_requests: watch::Sender<Option<u64>>,
    refresh_events: broadcast::Sender<DeviceRefreshEvent>,
    refresh_gate: AsyncMutex<()>,
    cache: Mutex<Cache>,
}

struct StatusRefresh {
    endpoint_id: String,
    generation: u64,
    deadline: tokio::time::Instant,
    events: broadcast::Sender<DeviceRefreshEvent>,
    _observer: watch::Receiver<ServiceSnapshot>,
}

impl StatusRefresh {
    fn publish(&self, phase: DeviceRefreshPhase) {
        let _ = self.events.send(DeviceRefreshEvent {
            endpoint_id: self.endpoint_id.clone(),
            generation: self.generation,
            phase,
        });
    }
}

fn refresh_deadline(
    refresh: Option<&StatusRefresh>,
) -> impl std::future::Future<Output = ()> + use<> {
    let deadline = refresh.map(|request| request.deadline);
    async move {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
}

struct StatusStream {
    request: CancelOnDrop,
    ended: oneshot::Receiver<Result<(), ResponsError>>,
}

impl StatusStream {
    async fn close(stream: &mut Option<Self>) {
        if let Some(stream) = stream.take() {
            stream.request.0.cancel();
            let _ = stream.ended.await;
        }
    }
}

impl ServiceProxy {
    pub fn new(network: Arc<Network>, runtime: tokio::runtime::Handle) -> Arc<Self> {
        let (refresh_requests, requests) = watch::channel(None);
        let proxy = Arc::new(Self {
            network,
            runtime,
            state: watch::Sender::new(ServiceSnapshot::default()),
            requested: Arc::new(Notify::new()),
            refresh_requests,
            refresh_events: broadcast::channel(16).0,
            refresh_gate: AsyncMutex::new(()),
            cache: Mutex::new(Cache::default()),
        });
        let worker = Arc::downgrade(&proxy);
        let targets = proxy.network.target_changes();
        proxy
            .runtime
            .spawn(Self::monitor(worker, targets, requests));
        proxy
    }

    pub(crate) fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        self.runtime.spawn(future);
    }

    pub fn snapshot(&self) -> ServiceSnapshot {
        self.sync_target();
        self.state.borrow().clone()
    }

    pub fn subscribe<T, P, F>(&self, project: P, changed: F) -> StatusSubscription
    where
        T: Clone + PartialEq + Send + 'static,
        P: Fn(&ServiceSnapshot) -> T + Send + 'static,
        F: FnMut(T) -> bool + Send + 'static,
    {
        StatusSubscription {
            task: self.listen(project, changed),
        }
    }

    pub fn listen<T, P, F>(&self, project: P, mut changed: F) -> JoinHandle<()>
    where
        T: Clone + PartialEq + Send + 'static,
        P: Fn(&ServiceSnapshot) -> T + Send + 'static,
        F: FnMut(T) -> bool + Send + 'static,
    {
        let mut state = self.status_updates();
        let mut targets = self.network.target_changes();
        let network = self.network.clone();
        self.runtime.spawn(async move {
            let mut previous = None;
            loop {
                let snapshot = state.borrow_and_update().clone();
                if network.generation() == snapshot.generation {
                    let value = project(&snapshot);
                    if previous.as_ref() != Some(&value) {
                        previous = Some(value.clone());
                        if !changed(value) {
                            break;
                        }
                    }
                }
                tokio::select! {
                    result = state.changed() => { if result.is_err() { break; } }
                    _ = targets.changed() => {
                        if network.with_target(|_, _, closed| closed) { break; }
                    }
                }
            }
        })
    }

    pub(crate) fn status_updates(&self) -> watch::Receiver<ServiceSnapshot> {
        self.sync_target();
        let idle = self.state.receiver_count() == 0;
        let state = self.state.subscribe();
        if idle {
            self.request_refresh();
        }
        state
    }

    pub fn request_refresh(&self) {
        self.requested.notify_one();
    }

    pub fn sync_target(&self) {
        self.network.with_target(|generation, target, _| {
            self.state.send_if_modified(|state| {
                if state.generation == generation {
                    return false;
                }
                *state = ServiceSnapshot {
                    generation,
                    endpoint_id: target.map(|(id, _)| id.to_string()),
                    ..ServiceSnapshot::default()
                };
                true
            });
            self.with_cache(generation, |_| ());
        });
    }

    pub(crate) fn publish_local<T: Serialize>(
        &self,
        generation: u64,
        module: &str,
        key: &str,
        value: &T,
    ) {
        let Ok(value) = serde_json::to_value(value) else {
            return;
        };
        self.network.with_target(|current, _, _| {
            if current != generation {
                return;
            }
            self.state.send_if_modified(|state| {
                if state.generation != generation {
                    return false;
                }
                let module = state.local.entry(module.to_owned()).or_default();
                if module.get(key) == Some(&value) {
                    return false;
                }
                module.insert(key.to_owned(), value);
                true
            });
        });
    }

    async fn monitor(
        proxy: Weak<Self>,
        mut targets: watch::Receiver<u64>,
        mut requests: watch::Receiver<Option<u64>>,
    ) {
        let mut refresh: Option<StatusRefresh> = None;
        let mut stream = None;
        while let Some(current) = proxy.upgrade() {
            targets.borrow_and_update();
            current.sync_target();
            let (generation, target, closed) = current
                .network
                .with_target(|generation, target, closed| (generation, target, closed));
            if closed {
                if let Some(request) = refresh.take() {
                    request.publish(DeviceRefreshPhase::Failed {
                        error: CancelledReason::NetworkClosed.into(),
                    });
                }
                break;
            }
            if refresh
                .as_ref()
                .is_some_and(|request| request.generation != generation)
                && let Some(request) = refresh.take()
            {
                request.publish(DeviceRefreshPhase::Failed {
                    error: CancelledReason::ServiceChanged.into(),
                });
            }
            let observed = current.state.receiver_count() > 0;
            let requested = current.requested.clone();
            let state = current.state.clone();
            let network = current.network.clone();
            let events = current.refresh_events.clone();
            drop(current);

            let work = async {
                match target.as_ref().map(|(_, binding)| binding) {
                    Some(BindingState::Binding) => {
                        tokio::select! {
                            _ = network.wait_for_binding(generation) => {}
                            _ = refresh_deadline(refresh.as_ref()) => {
                                if let Some(request) = refresh.take() {
                                    request.publish(DeviceRefreshPhase::Failed { error: NetworkReason::RequestTimeout.into() });
                                }
                            }
                        }
                    }
                    Some(BindingState::Bound { .. }) => {
                        Self::watch_status(&proxy, &network, generation, &mut stream, &mut refresh)
                            .await;
                    }
                    Some(BindingState::Failed { error }) => {
                        if let Some(request) = refresh.take() {
                            request.publish(DeviceRefreshPhase::Failed {
                                error: error.clone(),
                            });
                        }
                        if let Some(current) = proxy.upgrade() {
                            current.apply_status(generation, None, Err(error.clone()));
                        }
                        std::future::pending().await
                    }
                    _ => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                result = targets.changed() => { if result.is_err() { break; } }
                result = async {
                    loop {
                        requests.changed().await?;
                        if *requests.borrow_and_update() == Some(generation) {
                            break Ok::<(), watch::error::RecvError>(());
                        }
                    }
                } => {
                    if result.is_err() { break; }
                    if let Some((endpoint, binding)) = target {
                        let request = StatusRefresh {
                            generation,
                            endpoint_id: endpoint.to_string(),
                            deadline: tokio::time::Instant::now() + RequestOptions::default().timeout,
                            events,
                            _observer: state.subscribe(),
                        };
                        if matches!(binding, BindingState::Binding) {
                            request.publish(DeviceRefreshPhase::Registering);
                        }
                        refresh = Some(request);
                    }
                }
                _ = requested.notified() => {}
                _ = state.closed(), if observed => {}
                _ = work, if observed => {}
            }
            StatusStream::close(&mut stream).await;
        }
        StatusStream::close(&mut stream).await;
    }

    async fn watch_status(
        proxy: &Weak<Self>,
        network: &Network,
        generation: u64,
        stream: &mut Option<StatusStream>,
        refresh: &mut Option<StatusRefresh>,
    ) {
        let mut retry = Duration::from_secs(1);
        if let Some(request) = refresh.as_ref() {
            request.publish(DeviceRefreshPhase::Reconnecting);
        }
        loop {
            let relay = tokio::select! {
                result = network.wait_for_relay(generation) => result,
                _ = refresh_deadline(refresh.as_ref()) => Err(NetworkReason::RequestTimeout.into()),
            };
            let (mut error, received) = match relay {
                Ok(()) => {
                    if let Some(request) = refresh.as_ref() {
                        request.publish(DeviceRefreshPhase::Querying);
                    }
                    Self::subscribe_status(proxy, network, generation, stream, refresh).await
                }
                Err(error) => (error, false),
            };
            StatusStream::close(stream).await;
            if matches!(&error, ResponsError::Services(error) if error.code == "sse-not-supported")
            {
                // 旧服务使用单次查询，连接状态仍由网络事件更新。
                let Some(current) = proxy.upgrade() else {
                    return;
                };
                let result = tokio::select! {
                    result = current.request::<StatusResponse>(generation, ServiceRequest::get("/status")) => {
                        result.and_then(|response| response.data.into_status())
                    }
                    _ = refresh_deadline(refresh.as_ref()) => Err(NetworkReason::RequestTimeout.into()),
                };
                error = match result {
                    Ok(status) => {
                        current.apply_status(generation, None, Ok(status));
                        if let Some(request) = refresh.take() {
                            request.publish(DeviceRefreshPhase::Succeeded);
                        }
                        drop(current);
                        network.wait_for_link_loss(generation).await
                    }
                    Err(failure) => failure,
                };
            }
            let revoked = matches!(&error, ResponsError::NotBound(_));
            let cancelled = matches!(&error, ResponsError::Cancelled(_));
            let manual_failure = if let Some(request) = refresh.take() {
                request.publish(DeviceRefreshPhase::Failed {
                    error: error.clone(),
                });
                true
            } else {
                false
            };
            if cancelled {
                return;
            }
            let Some(current) = proxy.upgrade() else {
                return;
            };
            if !manual_failure || revoked {
                current.apply_status(generation, None, Err(error));
            }
            if revoked {
                return;
            }
            drop(current);
            if received {
                retry = Duration::from_secs(1);
            }
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(Duration::from_secs(30));
        }
    }

    async fn subscribe_status(
        proxy: &Weak<Self>,
        network: &Network,
        generation: u64,
        stream: &mut Option<StatusStream>,
        refresh: &mut Option<StatusRefresh>,
    ) -> (ResponsError, bool) {
        let (updates, mut snapshots) = watch::channel(None);
        let (done, ended) = oneshot::channel();
        let request = CancelOnDrop(network.request_sse_for::<StatusEvent>(
            Some(generation),
            ServiceRequest::get("/status"),
            SseRequestOptions::default(),
            true,
            move |update| {
                if let SseUpdate::Event(StatusEvent::Status { data }) = update {
                    updates.send_replace(Some(data.into_status()));
                }
            },
            move |result| {
                let _ = done.send(result);
            },
        ));
        let active = stream.insert(StatusStream { request, ended });
        let mut received = false;
        loop {
            let result = tokio::select! {
                biased;
                Ok(()) = snapshots.changed() => {
                    let Some(current) = proxy.upgrade() else {
                        return (CancelledReason::UserRequested.into(), received);
                    };
                    if let Some(result) = snapshots.borrow_and_update().clone() {
                        match result {
                            Ok(status) => {
                                current.apply_status(generation, None, Ok(status));
                                if let Some(request) = refresh.take() {
                                    request.publish(DeviceRefreshPhase::Succeeded);
                                }
                                received = true;
                            }
                            Err(error) => return (error, received),
                        }
                    }
                    continue;
                }
                _ = refresh_deadline(refresh.as_ref()) => {
                    return (NetworkReason::RequestTimeout.into(), received);
                }
                result = &mut active.ended => result,
            };
            stream.take();
            let error = result
                .ok()
                .and_then(Result::err)
                .unwrap_or_else(|| NetworkReason::ReceiveFailed.into());
            return (error, received);
        }
    }

    pub async fn refresh_status(&self) -> ServiceSnapshot {
        self.refresh_status_inner(None).await
    }

    pub fn refresh_device(&self, endpoint: Option<&str>) -> Result<(), ResponsError> {
        self.network.with_target(|generation, target, closed| {
            if closed {
                return Err(CancelledReason::NetworkClosed.into());
            }
            let (id, binding) = target.ok_or(NotConfiguredReason::ServiceNotSet)?;
            if endpoint.is_some_and(|endpoint| id.to_string() != endpoint) {
                return Err(CancelledReason::ServiceChanged.into());
            }
            if matches!(binding, BindingState::Unbound) {
                return Err(NotBoundReason::BindingRequired.into());
            }
            self.refresh_requests.send_replace(Some(generation));
            Ok(())
        })
    }

    pub fn watch_refresh(
        &self,
        mut changed: impl FnMut(DeviceRefreshEvent) -> bool + Send + 'static,
    ) -> StatusSubscription {
        let mut events = self.refresh_events.subscribe();
        let task = self.runtime.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if !changed(event) {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        StatusSubscription { task }
    }

    pub(crate) async fn refresh_status_for(
        &self,
        generation: u64,
    ) -> Result<ServiceSnapshot, ResponsError> {
        let snapshot = self.refresh_status_inner(Some(generation)).await;
        if snapshot.generation != generation {
            return Err(CancelledReason::ServiceChanged.into());
        }
        Ok(snapshot)
    }

    async fn refresh_status_inner(&self, expected: Option<u64>) -> ServiceSnapshot {
        let _gate = self.refresh_gate.lock().await;
        self.sync_target();
        let (generation, binding) = self
            .network
            .with_target(|generation, target, _| (generation, target.map(|(_, binding)| binding)));
        if expected.is_some_and(|expected| expected != generation) {
            return self.snapshot();
        }
        let revision = self.state.borrow().status_revision;
        let result = match binding {
            Some(BindingState::Bound { .. }) => self
                .request::<StatusResponse>(generation, ServiceRequest::get("/status"))
                .await
                .and_then(|response| response.data.into_status()),
            Some(BindingState::Failed { error }) => Err(error),
            _ => return self.snapshot(),
        };
        self.apply_status(generation, Some(revision), result);
        self.snapshot()
    }

    fn apply_status(
        &self,
        generation: u64,
        revision: Option<u64>,
        result: Result<StatusValues, ResponsError>,
    ) {
        let checked_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.network.with_target(|current, _, _| {
            if current == generation {
                let revoked = matches!(result, Err(ResponsError::NotBound(_)));
                if revoked {
                    self.with_cache(generation, |cache| {
                        *cache = Cache {
                            generation,
                            ..Cache::default()
                        }
                    });
                }
                self.state.send_if_modified(|state| {
                    if state.generation != generation
                        || (!revoked
                            && revision.is_some_and(|revision| state.status_revision != revision))
                    {
                        return false;
                    }
                    let mut next = state.clone();
                    next.checked_at = checked_at;
                    match result {
                        Ok(status) => {
                            next.status = Some(status);
                            next.error = None;
                        }
                        Err(error) => {
                            if revoked {
                                next.status = None;
                                next.local.clear();
                            }
                            next.error = Some(error);
                        }
                    }
                    if *state == next && next.error.is_some() {
                        return false;
                    }
                    next.status_revision += 1;
                    *state = next;
                    true
                });
            }
        });
    }

    pub async fn request<T: DeserializeOwned + Send + 'static>(
        &self,
        generation: u64,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
    ) -> Result<ServiceResponse<T>, ResponsError> {
        let (sender, receiver) = oneshot::channel();
        let sender = Mutex::new(Some(sender));
        let sender = Arc::new(sender);
        let error = Arc::clone(&sender);
        let handle = self.network.request_for(
            Some(generation),
            request,
            RequestOptions::default(),
            move |response| {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(Ok(response));
                }
            },
            move |response| {
                if let Some(sender) = error.lock().unwrap().take() {
                    let _ = sender.send(Err(response));
                }
            },
        );
        let _request = CancelOnDrop(handle);
        receiver
            .await
            .map_err(|_| ResponsError::from(NetworkReason::ReceiveFailed))?
    }

    pub fn request_sse<T: SseEventType + Send + 'static>(
        &self,
        generation: u64,
        request: ServiceRequest<impl Serialize + Send + Sync + 'static>,
        update: impl FnMut(SseUpdate<T>) + Send + 'static,
        end: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> RequestHandle {
        self.network.request_sse_for(
            Some(generation),
            request,
            SseRequestOptions::default(),
            false,
            update,
            end,
        )
    }

    pub async fn api_metadata(
        &self,
        generation: u64,
        module: &str,
    ) -> Result<ApiModuleMetadata, ResponsError> {
        if let Some(value) = self.cached(generation, |cache| cache.apis.get(module).cloned())? {
            return Ok(value);
        }
        let path = module_path("/discover/modules/", module)?;
        let value = self
            .request::<ApiModuleMetadata>(generation, ServiceRequest::get(path))
            .await?
            .data;
        if value.id != module {
            return Err(NetworkReason::InvalidResponse.into());
        }
        self.cached(generation, |cache| {
            cache.apis.insert(module.to_owned(), value.clone())
        })?;
        Ok(value)
    }

    pub async fn config_metadata(&self, generation: u64) -> Result<ConfigMetadata, ResponsError> {
        if let Some(value) = self.cached(generation, |cache| cache.config_meta.clone())? {
            return Ok(value);
        }
        let value = self
            .request::<ConfigMetadata>(generation, ServiceRequest::get("/meta"))
            .await?
            .data;
        self.cached(generation, |cache| cache.config_meta = Some(value.clone()))?;
        Ok(value)
    }

    pub async fn status_metadata(&self, generation: u64) -> Result<StatusMetadata, ResponsError> {
        if let Some(value) = self.cached(generation, |cache| cache.status_meta.clone())? {
            return Ok(value);
        }
        let value = self
            .request::<StatusMetadata>(generation, ServiceRequest::get("/stats/meta"))
            .await?
            .data;
        self.cached(generation, |cache| cache.status_meta = Some(value.clone()))?;
        Ok(value)
    }

    pub fn cached_config(
        &self,
        generation: u64,
        module: &str,
    ) -> Result<Option<ModuleConfig>, ResponsError> {
        self.cached(generation, |cache| cache.config.get(module).cloned())
    }

    pub async fn refresh_config(
        &self,
        generation: u64,
        module: &str,
    ) -> Result<ModuleConfig, ResponsError> {
        let path = module_path("/data/", module)?;
        let value = self
            .request::<ModuleConfig>(generation, ServiceRequest::get(path))
            .await?
            .data;
        if value.module_id != module {
            return Err(NetworkReason::InvalidResponse.into());
        }
        self.cached(generation, |cache| {
            cache.config.insert(module.to_owned(), value.clone())
        })?;
        Ok(value)
    }

    fn cached<T>(
        &self,
        generation: u64,
        action: impl FnOnce(&mut Cache) -> T,
    ) -> Result<T, ResponsError> {
        self.network.with_target(|current, target, closed| {
            if closed {
                return Err(CancelledReason::NetworkClosed.into());
            }
            if current != generation {
                return Err(CancelledReason::ServiceChanged.into());
            }
            require_bound(target.map(|(_, binding)| binding))?;
            Ok(self.with_cache(generation, action))
        })
    }

    pub(crate) fn bound_generation(&self, endpoint: &str) -> Result<u64, ResponsError> {
        self.network.with_target(|generation, target, closed| {
            if closed {
                return Err(CancelledReason::NetworkClosed.into());
            }
            let (id, binding) = target.ok_or(NotConfiguredReason::ServiceNotSet)?;
            if id.to_string() != endpoint {
                return Err(CancelledReason::ServiceChanged.into());
            }
            require_bound(Some(binding))?;
            Ok(generation)
        })
    }

    fn with_cache<T>(&self, generation: u64, action: impl FnOnce(&mut Cache) -> T) -> T {
        let mut cache = self.cache.lock().unwrap();
        if cache.generation != generation {
            *cache = Cache {
                generation,
                ..Cache::default()
            };
        }
        action(&mut cache)
    }
}

impl Drop for ServiceProxy {
    fn drop(&mut self) {
        self.requested.notify_one();
    }
}

fn require_bound(binding: Option<BindingState>) -> Result<(), ResponsError> {
    match binding {
        Some(BindingState::Bound { .. }) => Ok(()),
        Some(BindingState::Failed { error }) => Err(error),
        Some(_) => Err(NotBoundReason::BindingRequired.into()),
        None => Err(NotConfiguredReason::ServiceNotSet.into()),
    }
}

struct CancelOnDrop(RequestHandle);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn module_path(prefix: &str, module: &str) -> Result<String, ResponsError> {
    if module.is_empty()
        || !module
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(InvalidRequestReason::InvalidPath.into());
    }
    Ok(format!("{prefix}{module}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn listener_deduplicates_selected_values_and_drops_old_target_data() {
        let network = Arc::new(Network::new());
        let proxy = ServiceProxy::new(network.clone(), tokio::runtime::Handle::current());
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let listener = proxy.listen(
            |state| (state.generation, state.local::<u64>("test", "value")),
            move |value| send.send(value).is_ok(),
        );
        let update = async |receive: &mut tokio::sync::mpsc::UnboundedReceiver<_>| {
            tokio::time::timeout(Duration::from_secs(2), receive.recv())
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(update(&mut receive).await, (0, None));
        proxy.publish_local(0, "test", "value", &42);
        assert_eq!(update(&mut receive).await, (0, Some(42)));
        proxy.publish_local(0, "test", "something_else", &true);
        network.clear_service();
        proxy.sync_target();
        proxy.publish_local(0, "test", "value", &100);
        assert_eq!(update(&mut receive).await, (1, None));
        assert_eq!(proxy.snapshot().local::<u64>("test", "value"), None);
        network.shutdown().await;
        listener.await.unwrap();
    }

    #[test]
    fn absent_sse_metadata_is_compatible_with_older_services() {
        let metadata: ApiMetadata = serde_json::from_value(json!({
            "id": "start-sync", "method": "POST", "path": "/mail/sync"
        }))
        .unwrap();
        assert!(!metadata.supports_sse);
        assert!(metadata.sse_events.is_empty());
    }
}
