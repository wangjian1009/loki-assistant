#[cfg(test)]
use super::config::{relay_password, relay_token_is_set};
#[cfg(test)]
use crate::modules::config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler};
use std::{fmt, sync::Arc, time::Duration};
#[cfg(test)]
use std::{future::Future, pin::Pin};

use tokio::{
    sync::{Notify, mpsc, oneshot, watch},
    time::timeout,
};

use super::{
    config::relay_token,
    identity,
    pairing::PairingService,
    protocol::ConnectionStatus,
    relay::{RelayRegistrar, RelaySession},
};
use crate::modules::{
    alert::{AlertInput, AlertLevel, AlertService},
    config::service::ConfigService,
    notification::{NotificationService, protocol::NotificationRequest},
    status::StatusService,
};
use crate::process::Process;

const RELAY_REGISTRATION_ALERT: &str = "network.relay-registration";

#[derive(Clone, PartialEq, Eq)]
struct RelaySettings {
    url: String,
    token: String,
}

impl RelaySettings {
    fn configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.token.trim().is_empty()
    }
}

impl fmt::Debug for RelaySettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelaySettings")
            .field("url", &self.url)
            .field("token_configured", &!self.token.is_empty())
            .finish()
    }
}

#[derive(Debug)]
struct Registration {
    settings: RelaySettings,
    endpoint_id: String,
    session: Arc<dyn RelaySession>,
}

enum Candidate {
    Keep,
    Replace(Option<Registration>),
}

#[derive(Debug, Default)]
struct State {
    active: Option<Registration>,
    registering: bool,
    phase: ConnectionStatus,
}

enum RelayCommand {
    Start(oneshot::Sender<Result<(), String>>),
    Shutdown(oneshot::Sender<()>),
}

/// 本机和配置模块使用的网络服务句柄。
#[derive(Clone, Debug)]
pub struct NetworkService {
    context: RelayContext,
    commands: mpsc::UnboundedSender<RelayCommand>,
    settings_changed: watch::Sender<()>,
    pub(super) pairings: Arc<PairingService>,
}

#[derive(Clone, Debug)]
pub(super) struct RelayContext {
    pub(super) remote: Arc<super::remote::Gateway>,
    config: ConfigService,
    registrar: Arc<dyn RelayRegistrar>,
    notification: Arc<NotificationService>,
    alert: Arc<AlertService>,
    state: Process<State>,
    registration_timeout: Duration,
    status: Arc<StatusService>,
    connection_changed: Arc<Notify>,
    pairing_changed: Arc<Notify>,
    pub(super) pairings: Arc<PairingService>,
}

impl NetworkService {
    pub fn router(self: &Arc<Self>) -> axum::Router {
        super::routes::router(self.clone())
    }

    pub async fn create_pairing(
        &self,
    ) -> Result<super::protocol::CreatePairingResponse, super::protocol::NetworkError> {
        use super::protocol::NetworkError;
        let service = self.context.clone();
        self.context
            .state
            .with(move |state| {
                Box::pin(async move {
                    let active = state
                        .active
                        .as_ref()
                        .filter(|active| active.session.status() == ConnectionStatus::Connected)
                        .ok_or(NetworkError::NotReady)?;
                    service
                        .pairings
                        .create(
                            active.endpoint_id.clone(),
                            active.settings.url.clone(),
                            active.settings.token.clone(),
                        )
                        .await
                })
            })
            .await
    }

    pub async fn pairing(
        &self,
        id: &str,
        cancel: bool,
    ) -> Result<super::protocol::PairingResponse, super::protocol::NetworkError> {
        self.pairings.query(id, cancel).await
    }

    pub async fn peers(&self) -> Result<Vec<super::protocol::Peer>, super::protocol::NetworkError> {
        self.pairings.list().await
    }

    pub async fn remove_peer(
        &self,
        id: iroh::EndpointId,
    ) -> Result<(), super::protocol::NetworkError> {
        self.pairings.remove(id).await
    }

    pub(super) async fn peer_session(
        &self,
        id: iroh::EndpointId,
    ) -> Result<serde_json::Value, super::protocol::NetworkError> {
        self.context.peer_session(id).await
    }

    pub fn new(
        config: ConfigService,
        registrar: Arc<dyn RelayRegistrar>,
        notification: Arc<NotificationService>,
        alert: Arc<AlertService>,
        status: Arc<StatusService>,
        registration_timeout: Duration,
    ) -> Self {
        status.post(
            "network",
            "my",
            serde_json::json!({"status": ConnectionStatus::Offline}),
        );
        let pairings = Arc::new(PairingService::new(config.clone()));
        let (state, state_requests) = Process::channel(State::default());
        let (commands, inbox) = mpsc::unbounded_channel();
        let (settings_changed, changes) = watch::channel(());
        let context = RelayContext {
            remote: Arc::new(super::remote::Gateway::new()),
            config,
            registrar,
            notification,
            alert,
            state,
            registration_timeout,
            status,
            connection_changed: Arc::new(Notify::new()),
            pairing_changed: Arc::new(Notify::new()),
            pairings: pairings.clone(),
        };
        let relay = context.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = relay.run(inbox, changes) => {}
                _ = state_requests => {}
                _ = relay.monitor() => {}
                _ = relay.receive_pairings() => {}
            }
        });
        Self {
            context,
            commands,
            settings_changed,
            pairings,
        }
    }

    pub async fn start(&self) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RelayCommand::Start(reply))
            .map_err(|_| "中转任务已关闭".to_owned())?;
        response.await.map_err(|_| "中转任务已关闭".to_owned())?
    }

    #[cfg(test)]
    pub(super) fn config(&self) -> ConfigService {
        self.context.config.clone()
    }

    pub async fn shutdown(&self) {
        let (reply, response) = oneshot::channel();
        if self.commands.send(RelayCommand::Shutdown(reply)).is_ok() {
            let _ = response.await;
        }
    }

    pub(super) fn configured_relay_changed(&self) {
        if self.settings_changed.send(()).is_err() {
            log::warn!("中转任务已关闭，无法应用网络配置。");
        }
    }

    pub fn set_remote_dispatcher(&self, dispatcher: Arc<dyn super::remote::RequestDispatcher>) {
        self.context.remote.set_dispatcher(dispatcher);
    }
}

impl RelayContext {
    async fn run(
        &self,
        mut inbox: mpsc::UnboundedReceiver<RelayCommand>,
        mut changes: watch::Receiver<()>,
    ) {
        let mut result = None;
        loop {
            tokio::select! {
                command = inbox.recv() => match command {
                    Some(RelayCommand::Start(reply)) => {
                        if result.is_none() || changes.has_changed().unwrap_or(false) {
                            result = Some(self.reload(&mut changes).await);
                        }
                        let _ = reply.send(result.clone().expect("中转初始化已完成"));
                    }
                    Some(RelayCommand::Shutdown(reply)) => {
                        self.shutdown().await;
                        let _ = reply.send(());
                        return;
                    }
                    None => break,
                },
                change = changes.changed() => {
                    if change.is_err() {
                        break;
                    }
                    result = Some(self.reload(&mut changes).await);
                }
            }
        }
        self.shutdown().await;
    }

    async fn reload(&self, changes: &mut watch::Receiver<()>) -> Result<(), String> {
        loop {
            changes.borrow_and_update();
            if let Err(error) = self.pairings.initialize().await {
                log::warn!("加载设备绑定失败：{error}");
            }
            let settings: Result<_, String> = async {
                let url = self.relay_url().await?;
                let token = relay_token(&self.config)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(RelaySettings { url, token })
            }
            .await;
            let result = match settings {
                Ok(settings) => self.apply(settings, changes).await,
                Err(error) => {
                    let status = self.status.clone();
                    self.state
                        .call(move |state| {
                            Self::restore_active_phase(&status, state, ConnectionStatus::Failed);
                        })
                        .await;
                    if !changes.has_changed().unwrap_or(false) {
                        self.report_registration_failure(format!("读取中转配置失败：{error}"))
                            .await;
                    }
                    Err(error)
                }
            };
            if !changes.has_changed().unwrap_or(false) {
                return result;
            }
        }
    }

    async fn receive_pairings(&self) {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let changed = self.pairing_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let session = self
                .state
                .call(|state| state.active.as_ref().map(|active| active.session.clone()))
                .await;
            let endpoint = session.as_ref().and_then(|session| session.endpoint());
            tokio::select! {
                _ = &mut changed => {},
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                incoming = async { endpoint.as_ref().unwrap().accept().await }, if endpoint.is_some() && tasks.len() < 32 => {
                    if let Some(incoming) = incoming {
                        let service = self.clone();
                        let session = session.unwrap();
                        tasks.spawn(async move {
                            let connection = match tokio::time::timeout(Duration::from_secs(10), incoming).await {
                                Ok(Ok(connection)) => connection,
                                _ => return,
                            };
                            service.pairings.track(connection.clone()).await;
                            if connection.alpn() == super::protocol::SERVICES_ALPN {
                                service.clone().handle_remote(&session, &connection).await;
                            } else {
                                let _ = tokio::time::timeout(Duration::from_secs(15), service.handle_pairing(&session, &connection)).await;
                            }
                            service.pairings.untrack(connection.stable_id()).await;
                            connection.close(0u32.into(), b"finished");
                        });
                    } else {
                        changed.await;
                    }
                }
            }
        }
    }

    async fn handle_pairing(
        &self,
        session: &Arc<dyn RelaySession>,
        connection: &iroh::endpoint::Connection,
    ) -> Result<(), ()> {
        use super::protocol::{BindRequest, DEVICE_STATUS_ALPN, DeviceStatusRequest, NetworkError};
        let local_id = session.endpoint().ok_or(())?.id();
        let (mut send, mut recv) = connection.accept_bi().await.map_err(|_| ())?;
        let result =
            match recv.read_to_end(16_384).await {
                Ok(data) if connection.alpn() == DEVICE_STATUS_ALPN => {
                    match serde_json::from_slice::<DeviceStatusRequest>(&data) {
                        Ok(request)
                            if request.api_version == "1" && request.kind == "device.status" =>
                        {
                            let session = session.clone();
                            let remote = connection.remote_id();
                            let service = self.clone();
                            self.state
                                .with(move |state| {
                                    Box::pin(async move {
                                        if !state.active.as_ref().is_some_and(|active| {
                                            Arc::ptr_eq(&active.session, &session)
                                        }) {
                                            Err(NetworkError::NotReady)
                                        } else {
                                            match service
                                .pairings
                                .authenticate(remote, &request.binding_token)
                                .await
                            {
                                Err(error) => Err(error),
                                Ok(()) => service.pairings.list().await.and_then(|peers| {
                                    let peer = peers
                                        .iter()
                                        .find(|peer| {
                                            peer.endpoint_id == remote.to_string()
                                        })
                                        .ok_or(NetworkError::PeerNotBound)?;
                                    Ok(serde_json::json!({
                                        "api_version": "1", "type": "device.status",
                                        "endpoint_id": local_id.to_string(),
                                        "status": "online", "bound_at": peer.bound_at,
                                    }))
                                }),
                            }
                                        }
                                    })
                                })
                                .await
                        }
                        _ => Err(NetworkError::InvalidRequest),
                    }
                }
                Ok(data) => match serde_json::from_slice::<BindRequest>(&data) {
                    Ok(request) => {
                        let session = session.clone();
                        let remote = connection.remote_id();
                        let service = self.clone();
                        self.state
                            .with(move |state| {
                                Box::pin(async move {
                                    if !state.active.as_ref().is_some_and(|active| {
                                        Arc::ptr_eq(&active.session, &session)
                                    }) {
                                        Err(NetworkError::NotReady)
                                    } else {
                                        service.pairings.bind(remote, request).await.map(
                                            |response| {
                                                serde_json::to_value(response)
                                                    .expect("绑定响应可序列化")
                                            },
                                        )
                                    }
                                })
                            })
                            .await
                    }
                    Err(_) => Err(NetworkError::InvalidRequest),
                },
                Err(_) => Err(NetworkError::InvalidRequest),
            };
        let response = match result {
            Ok(response) => serde_json::to_vec(&response).map_err(|_| ())?,
            Err(error) => serde_json::to_vec(&crate::http::error::ApiErrorResponse {
                api_version: super::protocol::API_VERSION.to_owned(),
                status: error.status(),
                error: crate::http::error::ApiError {
                    source: crate::http::error::ErrorSource::Business,
                    code: error.code().to_owned(),
                    message: error.to_string(),
                },
            })
            .map_err(|_| ())?,
        };
        send.write_all(&response).await.map_err(|_| ())?;
        send.finish().map_err(|_| ())?;
        let _ = send.stopped().await;
        Ok(())
    }

    pub(super) async fn authenticated_peer(
        &self,
        session: &Arc<dyn RelaySession>,
        id: iroh::EndpointId,
    ) -> Result<super::protocol::Peer, super::protocol::NetworkError> {
        use super::protocol::NetworkError;
        let session = session.clone();
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    if !state
                        .active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(&active.session, &session))
                    {
                        return Err(NetworkError::NotReady);
                    }
                    service
                        .pairings
                        .list()
                        .await?
                        .into_iter()
                        .find(|peer| peer.endpoint_id == id.to_string())
                        .ok_or(NetworkError::PeerNotBound)
                })
            })
            .await
    }

    pub(super) async fn peer_session(
        &self,
        id: iroh::EndpointId,
    ) -> Result<serde_json::Value, super::protocol::NetworkError> {
        use super::protocol::NetworkError;
        let service = self.clone();
        self.state.with(move |state| Box::pin(async move {
        let active = state
            .active
            .as_ref()
            .ok_or(NetworkError::NotReady)?;
        let peer = service
            .pairings
            .list()
            .await?
            .into_iter()
            .find(|peer| peer.endpoint_id == id.to_string())
            .ok_or(NetworkError::PeerNotBound)?;
        let name = service
            .status
            .value_for("service", "name", true)
            .await
            .ok()
            .and_then(|v| v.value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "Loki 电脑".into());
        Ok(
            serde_json::json!({"api_version":"1","endpoint_id":active.endpoint_id,"status":"online","bound_at":peer.bound_at,"name":name,"device_type":"computer"}),
        )
        })).await
    }

    async fn notify(&self, title: &str, body: String) {
        match self
            .notification
            .send(NotificationRequest {
                title: title.to_owned(),
                body: Some(body),
                app: None,
                callback_url: None,
            })
            .await
        {
            Ok(receipt) => log::info!(
                "{title}：通知已登记，notification_id={}",
                receipt.notification_id
            ),
            Err(error) => log::warn!("{title}，通知登记失败：{error}"),
        }
    }

    async fn identity(&self) -> Result<iroh::SecretKey, String> {
        let (identity, created) = identity::ensure(&self.config).await?;
        if created {
            self.notify(
                "已生成本地身份",
                format!(
                    "身份私钥已保存到 GPG 密码库。\nEndpoint ID：{}",
                    identity.public()
                ),
            )
            .await;
        }
        Ok(identity)
    }

    async fn report_registration_failure(&self, error: String) {
        if let Err(error) = self
            .alert
            .publish(
                RELAY_REGISTRATION_ALERT,
                AlertInput {
                    app: None,
                    title: "中转注册失败".into(),
                    body: Some(error),
                    level: AlertLevel::Error,
                    callback_url: None,
                },
            )
            .await
        {
            log::warn!("发布中转注册告警失败：{error}");
        }
    }

    fn publish_phase(status: &StatusService, state: &mut State, phase: ConnectionStatus) {
        if state.phase != phase {
            state.phase = phase;
            status.post("network", "my", serde_json::json!({"status": phase}));
        }
    }

    fn restore_active_phase(status: &StatusService, state: &mut State, fallback: ConnectionStatus) {
        let phase = state
            .active
            .as_ref()
            .map(|active| active.session.status())
            .unwrap_or(fallback);
        Self::publish_phase(status, state, phase);
    }

    async fn apply(
        &self,
        settings: RelaySettings,
        changes: &watch::Receiver<()>,
    ) -> Result<(), String> {
        let configured = settings.configured();
        let status = self.status.clone();
        self.state
            .call(move |state| {
                state.registering = true;
                if configured {
                    Self::publish_phase(&status, state, ConnectionStatus::Connecting);
                }
            })
            .await;
        let candidate = self.register_candidate(settings).await;
        let status = self.status.clone();
        let pairings = self.pairings.clone();
        let pending_changes = changes.clone();
        let (old, outcome) = self
            .state
            .with(move |state| {
                Box::pin(async move {
                    state.registering = false;
                    if pending_changes.has_changed().unwrap_or(false) {
                        Self::restore_active_phase(&status, state, ConnectionStatus::Offline);
                        let stale = match candidate {
                            Ok(Candidate::Replace(value)) => value,
                            _ => None,
                        };
                        return (stale, None);
                    }
                    match candidate {
                        Ok(Candidate::Replace(current)) => {
                            pairings.invalidate().await;
                            let old = std::mem::replace(&mut state.active, current);
                            Self::restore_active_phase(&status, state, ConnectionStatus::Offline);
                            (old, Some(Ok(())))
                        }
                        Ok(Candidate::Keep) => {
                            Self::restore_active_phase(&status, state, ConnectionStatus::Connected);
                            (None, Some(Ok(())))
                        }
                        Err(error) => {
                            let fallback = if configured {
                                ConnectionStatus::Failed
                            } else {
                                ConnectionStatus::Offline
                            };
                            Self::restore_active_phase(&status, state, fallback);
                            (None, Some(Err(error)))
                        }
                    }
                })
            })
            .await;
        self.connection_changed.notify_one();
        self.pairing_changed.notify_one();
        if let Some(old) = old
            && let Err(error) = old.session.close().await
        {
            log::warn!("关闭中转连接失败：{error}");
        }
        if let Some(outcome) = &outcome
            && !changes.has_changed().unwrap_or(false)
        {
            match outcome {
                Ok(()) => {
                    if let Err(error) = self.alert.clear(RELAY_REGISTRATION_ALERT).await {
                        log::warn!("清理中转注册告警失败：{error}");
                    }
                }
                Err(error) if configured => self.report_registration_failure(error.clone()).await,
                Err(error) => self.notify("网络身份初始化失败", error.clone()).await,
            }
        }
        outcome.unwrap_or(Ok(()))
    }

    // 外部 I/O 在中转任务中执行，状态请求仍可查询当前连接。
    async fn register_candidate(&self, settings: RelaySettings) -> Result<Candidate, String> {
        let identity = self.identity().await?;
        if !settings.configured() {
            return Ok(Candidate::Replace(None));
        }
        let endpoint_id = identity.public().to_string();
        let existing_settings = settings.clone();
        let existing_id = endpoint_id.clone();
        let already_connected = self
            .state
            .call(move |state| {
                state.active.as_ref().is_some_and(|active| {
                    active.settings == existing_settings
                        && active.endpoint_id == existing_id
                        && active.session.status() == ConnectionStatus::Connected
                })
            })
            .await;
        if already_connected {
            return Ok(Candidate::Keep);
        }
        let result = timeout(
            self.registration_timeout,
            self.registrar
                .register(identity, settings.url.clone(), settings.token.clone()),
        )
        .await
        .map_err(|_| {
            format!(
                "中转注册超时（{} 秒），请检查地址、网络和访问令牌",
                self.registration_timeout.as_secs()
            )
        })
        .and_then(|result| result);
        match result {
            Ok(session) => Ok(Candidate::Replace(Some(Registration {
                settings,
                endpoint_id,
                session,
            }))),
            Err(error) => Err(error.replace(&settings.token, "[已隐藏]")),
        }
    }

    async fn relay_url(&self) -> Result<String, String> {
        self.config
            .value("network", "relay_url")
            .await
            .map_err(|error| error.to_string())?
            .value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "网络配置 relay_url 必须是字符串".into())
    }

    async fn monitor(&self) {
        loop {
            let changed = self.connection_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let active = self
                .state
                .call(|state| {
                    state
                        .active
                        .as_ref()
                        .filter(|_| !state.registering)
                        .map(|active| active.session.clone())
                })
                .await;
            if let Some(session) = active {
                let phase = session.status();
                Self::publish_session_phase(&self.state, &self.status, &session, phase).await;
                if phase == ConnectionStatus::Disconnected {
                    changed.await;
                    continue;
                }
                tokio::select! {
                    _ = &mut changed => {},
                    result = session.status_changed(phase) => {
                        if let Ok(next) = result { Self::publish_session_phase(&self.state, &self.status, &session, next).await; }
                    }
                }
            } else {
                changed.await;
            }
        }
    }

    async fn publish_session_phase(
        state: &Process<State>,
        status: &Arc<StatusService>,
        session: &Arc<dyn RelaySession>,
        phase: ConnectionStatus,
    ) {
        let session = session.clone();
        let status = status.clone();
        state
            .call(move |state| {
                if !state.registering
                    && state
                        .active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(&active.session, &session))
                {
                    Self::publish_phase(&status, state, phase);
                }
            })
            .await;
    }

    async fn shutdown(&self) {
        let status = self.status.clone();
        let pairings = self.pairings.clone();
        let active = self
            .state
            .with(move |state| {
                Box::pin(async move {
                    pairings.invalidate().await;
                    Self::publish_phase(&status, state, ConnectionStatus::Disconnected);
                    state.active.take()
                })
            })
            .await;
        if let Some(active) = active {
            let _ = active.session.close().await;
        }
    }
}

#[cfg(test)]
impl ConfigChangeHandler for NetworkService {
    fn secret_is_set<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(
            async move { key == "relay_token" && relay_token_is_set(&self.context.config).await },
        )
    }

    fn write_secret<'a>(&'a self, key: &'a str, value: serde_json::Value) -> ChangeFuture<'a> {
        Box::pin(async move {
            if key != "relay_token" {
                return Err("未找到中转凭据配置。".into());
            }
            let token = value.as_str().ok_or("中转令牌必须是字符串。")?;
            let config = &self.context.config;
            // 先导入旧令牌，避免换新后留下待迁移的旧值。
            relay_token(config)
                .await
                .map_err(|error| error.to_string())?;
            let password = relay_password();
            if token.trim().is_empty() {
                password.remove(config).await
            } else {
                password.write(config, token).await
            }
            .map_err(|error| error.to_string())
        })
    }

    fn committed(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network"
                && matches!(change.key.as_str(), "relay_url" | "relay_token")
            {
                self.settings_changed
                    .send(())
                    .map_err(|_| "中转任务已关闭".to_owned())?;
            }
            Ok(())
        })
    }
}
