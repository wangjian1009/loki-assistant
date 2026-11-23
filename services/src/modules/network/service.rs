use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use tokio::{sync::Notify, time::timeout};

use super::{
    config::{relay_password, relay_token, relay_token_is_set},
    identity,
    pairing::PairingService,
    protocol::ConnectionStatus,
    relay::{RelayRegistrar, RelaySession},
};
use crate::modules::{
    config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler, ConfigService},
    notification::{NotificationService, protocol::NotificationRequest},
    status::StatusService,
};
use crate::process::Process;

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

#[derive(Debug)]
struct PendingRegistration {
    candidate: Option<Registration>,
    previous_status: ConnectionStatus,
}

#[derive(Debug, Default)]
struct State {
    active: Option<Registration>,
    pending: Option<PendingRegistration>,
    registering: bool,
    phase: ConnectionStatus,
}

/// 持有当前中转连接；新地址完成注册并落盘后才替换原连接。
#[derive(Clone, Debug)]
pub struct NetworkService {
    pub(super) remote: Arc<super::remote::Gateway>,
    config: ConfigService,
    registrar: Arc<dyn RelayRegistrar>,
    notification: Arc<NotificationService>,
    state: Process<State>,
    relay: Process<()>,
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
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    let active = state
                        .active
                        .as_ref()
                        .filter(|active| {
                            state.pending.is_none()
                                && active.session.status() == ConnectionStatus::Connected
                        })
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

    /// 由进程入口显式调度，分发绑定、设备状态及复用的业务连接。
    pub async fn receive_pairings(self: Arc<Self>) {
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
                                        if state.pending.is_some()
                                            || !state.active.as_ref().is_some_and(|active| {
                                                Arc::ptr_eq(&active.session, &session)
                                            })
                                        {
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
                                    if state.pending.is_some()
                                        || !state.active.as_ref().is_some_and(|active| {
                                            Arc::ptr_eq(&active.session, &session)
                                        })
                                    {
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
                    if state.pending.is_some()
                        || !state
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
            .filter(|_| state.pending.is_none())
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

    pub fn new(
        config: ConfigService,
        registrar: Arc<dyn RelayRegistrar>,
        notification: Arc<NotificationService>,
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
        let (relay, commands) = Process::channel(());
        let connection_changed = Arc::new(Notify::new());
        let monitor_state = state.clone();
        let monitor_changed = connection_changed.clone();
        let monitor_status = status.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = commands => {}
                _ = state_requests => {}
                _ = Self::monitor(monitor_state, monitor_changed, monitor_status) => {}
            }
        });
        Self {
            remote: Arc::new(super::remote::Gateway::new()),
            config,
            registrar,
            notification,
            state,
            relay,
            registration_timeout,
            status,
            connection_changed,
            pairing_changed: Arc::new(Notify::new()),
            pairings,
        }
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
            Ok(receipt) => eprintln!(
                "{title}：通知已登记，notification_id={}",
                receipt.notification_id
            ),
            Err(error) => eprintln!("{title}，通知登记失败：{error}"),
        }
    }

    async fn identity(&self) -> Result<iroh::SecretKey, String> {
        let result = identity::ensure(&self.config).await;
        match result {
            Ok((identity, created)) => {
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
            Err(error) => {
                self.notify("网络身份初始化失败", error.clone()).await;
                Err(error)
            }
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

    async fn prepare(&self, settings: RelaySettings) -> Result<(), String> {
        let configured = settings.configured();
        let status = self.status.clone();
        let previous_status = self
            .state
            .call(move |state| {
                if state.pending.is_some() {
                    return Err("网络配置正在提交，请稍后重试".to_owned());
                }
                let previous = state.phase;
                state.registering = true;
                if configured {
                    Self::publish_phase(&status, state, ConnectionStatus::Connecting);
                }
                Ok(previous)
            })
            .await?;

        let candidate = self.register_candidate(settings).await;
        let status = self.status.clone();
        let outcome = self
            .state
            .call(move |state| {
                state.registering = false;
                match candidate {
                    Ok(Candidate::Replace(candidate)) => {
                        state.pending = Some(PendingRegistration {
                            candidate,
                            previous_status,
                        });
                        Ok(())
                    }
                    Ok(Candidate::Keep) => {
                        Self::restore_active_phase(&status, state, ConnectionStatus::Connected);
                        Ok(())
                    }
                    Err(error) => {
                        let fallback = if configured {
                            ConnectionStatus::Failed
                        } else {
                            ConnectionStatus::Offline
                        };
                        Self::restore_active_phase(&status, state, fallback);
                        Err(error)
                    }
                }
            })
            .await;
        self.connection_changed.notify_one();
        outcome
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
            Err(error) => {
                let error = error.replace(&settings.token, "[已隐藏]");
                self.notify("中转注册失败", error.clone()).await;
                Err(error)
            }
        }
    }

    async fn finish(&self) {
        let status = self.status.clone();
        let pairings = self.pairings.clone();
        let switched = self
            .state
            .with(move |state| {
                Box::pin(async move {
                    let pending = state.pending.take()?;
                    pairings.invalidate().await;
                    let old = std::mem::replace(&mut state.active, pending.candidate);
                    Self::restore_active_phase(&status, state, ConnectionStatus::Offline);
                    let notice = state.active.as_ref().map(|active| {
                        format!(
                            "中转地址：{}\nEndpoint ID：{}",
                            active.settings.url, active.endpoint_id
                        )
                    });
                    Some((old, notice))
                })
            })
            .await;
        if let Some((old, notice)) = switched {
            self.connection_changed.notify_one();
            self.pairing_changed.notify_one();
            if let Some(old) = old
                && let Err(error) = old.session.close().await
            {
                eprintln!("关闭原中转连接失败：{error}");
            }
            if let Some(notice) = notice {
                self.notify("中转注册成功", notice).await;
            }
        }
    }

    async fn discard_pending(&self) -> Result<(), String> {
        let status = self.status.clone();
        let pending = self
            .state
            .call(move |state| {
                let pending = state.pending.take()?;
                Self::publish_phase(&status, state, pending.previous_status);
                Some(pending)
            })
            .await;
        if let Some(pending) = pending {
            self.connection_changed.notify_one();
            if let Some(candidate) = pending.candidate {
                candidate.session.close().await?;
            }
            self.notify(
                "中转配置保存失败",
                "配置未能保存，已撤销本次注册并保留原连接。".to_owned(),
            )
            .await;
        }
        Ok(())
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

    async fn monitor(
        state: Process<State>,
        connection_changed: Arc<Notify>,
        status: Arc<StatusService>,
    ) {
        loop {
            let changed = connection_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let active = state
                .call(|state| {
                    state
                        .active
                        .as_ref()
                        .filter(|_| state.pending.is_none() && !state.registering)
                        .map(|active| active.session.clone())
                })
                .await;
            if let Some(session) = active {
                let phase = session.status();
                Self::publish_session_phase(&state, &status, &session, phase).await;
                if phase == ConnectionStatus::Disconnected {
                    changed.await;
                    continue;
                }
                tokio::select! {
                    _ = &mut changed => {},
                    result = session.status_changed(phase) => {
                        if let Ok(next) = result { Self::publish_session_phase(&state, &status, &session, next).await; }
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
                if state.pending.is_none()
                    && !state.registering
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

    /// 由进程入口显式调用；失败不影响本机 HTTP 服务，配置提交后仍可重试。
    pub async fn start(&self) -> Result<(), String> {
        if let Err(error) = self.pairings.initialize().await {
            eprintln!("加载设备绑定失败：{error}");
        }
        let service = self.clone();
        self.relay
            .run(async move {
                if service.state.call(|state| state.pending.is_some()).await {
                    return Ok(());
                }
                let url = service.relay_url().await?;
                let token = relay_token(&service.config)
                    .await
                    .map_err(|error| error.to_string())?;
                service.prepare(RelaySettings { url, token }).await?;
                service.finish().await;
                Ok(())
            })
            .await
    }

    #[cfg(test)]
    pub(super) fn config(&self) -> ConfigService {
        self.config.clone()
    }

    pub async fn shutdown(&self) {
        let service = self.clone();
        self.relay
            .run(async move {
                let status = service.status.clone();
                let pairings = service.pairings.clone();
                let (pending, active) = service
                    .state
                    .with(move |state| {
                        Box::pin(async move {
                            pairings.invalidate().await;
                            Self::publish_phase(&status, state, ConnectionStatus::Disconnected);
                            (state.pending.take(), state.active.take())
                        })
                    })
                    .await;
                service.connection_changed.notify_one();
                service.pairing_changed.notify_one();
                if let Some(candidate) = pending.and_then(|pending| pending.candidate) {
                    let _ = candidate.session.close().await;
                }
                if let Some(active) = active {
                    let _ = active.session.close().await;
                }
            })
            .await;
    }
}

impl ConfigChangeHandler for NetworkService {
    fn secret_is_set<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { key == "relay_token" && relay_token_is_set(&self.config).await })
    }

    fn write_secret<'a>(&'a self, key: &'a str, value: serde_json::Value) -> ChangeFuture<'a> {
        Box::pin(async move {
            if key != "relay_token" {
                return Err("未找到中转凭据配置。".into());
            }
            let token = value.as_str().ok_or("中转令牌必须是字符串。")?;
            let token = token.to_owned();
            let service = self.clone();
            self.relay
                .run(async move {
                    // 先导入旧令牌，避免换新后留下待迁移的旧值。
                    relay_token(&service.config)
                        .await
                        .map_err(|error| error.to_string())?;
                    let url = service.relay_url().await?;
                    service
                        .prepare(RelaySettings {
                            url,
                            token: token.to_owned(),
                        })
                        .await?;
                    let password = relay_password();
                    let saved = if token.trim().is_empty() {
                        password.remove(&service.config).await
                    } else {
                        password.write(&service.config, &token).await
                    };
                    if let Err(error) = saved {
                        service.discard_pending().await?;
                        return Err(error.to_string());
                    }
                    service.finish().await;
                    Ok(())
                })
                .await
        })
    }

    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id != "network" || change.key != "relay_url" {
                return Ok(());
            }
            let url = change
                .value
                .as_str()
                .ok_or("中转地址必须是字符串。")?
                .to_owned();
            let service = self.clone();
            self.relay
                .run(async move {
                    let token = relay_token(&service.config)
                        .await
                        .map_err(|error| error.to_string())?;
                    service.prepare(RelaySettings { url, token }).await
                })
                .await
        })
    }

    fn committed(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network" && change.key == "relay_url" {
                let service = self.clone();
                self.relay.run(async move { service.finish().await }).await;
            }
            Ok(())
        })
    }

    fn rollback(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network" && change.key == "relay_url" {
                let service = self.clone();
                self.relay
                    .run(async move { service.discard_pending().await })
                    .await?;
            }
            Ok(())
        })
    }
}
