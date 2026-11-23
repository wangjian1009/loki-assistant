use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, Notify},
    time::timeout,
};

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

#[derive(Debug)]
struct PendingRegistration {
    candidate: Option<Registration>,
    previous_status: ConnectionStatus,
}

#[derive(Debug, Default)]
struct State {
    active: Option<Registration>,
    pending: Option<PendingRegistration>,
    phase: ConnectionStatus,
}

/// 持有当前中转连接；新地址完成注册并落盘后才替换原连接。
#[derive(Debug)]
pub struct NetworkService {
    pub(super) remote: super::remote::Gateway,
    config: ConfigService,
    registrar: Arc<dyn RelayRegistrar>,
    notification: Arc<NotificationService>,
    state: Mutex<State>,
    registration_timeout: Duration,
    status: Arc<StatusService>,
    connection_changed: Notify,
    pairing_changed: Notify,
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
        let state = self.state.lock().await;
        let active = state
            .active
            .as_ref()
            .filter(|active| {
                state.pending.is_none() && active.session.status() == ConnectionStatus::Connected
            })
            .ok_or(NetworkError::NotReady)?;
        self.pairings
            .create(
                active.endpoint_id.clone(),
                active.settings.url.clone(),
                active.settings.token.clone(),
            )
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
                .lock()
                .await
                .active
                .as_ref()
                .map(|active| active.session.clone());
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
        let result = match recv.read_to_end(16_384).await {
            Ok(data) if connection.alpn() == DEVICE_STATUS_ALPN => {
                match serde_json::from_slice::<DeviceStatusRequest>(&data) {
                    Ok(request)
                        if request.api_version == "1" && request.kind == "device.status" =>
                    {
                        let state = self.state.lock().await;
                        if state.pending.is_some()
                            || !state
                                .active
                                .as_ref()
                                .is_some_and(|active| Arc::ptr_eq(&active.session, session))
                        {
                            Err(NetworkError::NotReady)
                        } else {
                            match self
                                .pairings
                                .authenticate(connection.remote_id(), &request.binding_token)
                                .await
                            {
                                Err(error) => Err(error),
                                Ok(()) => self.pairings.list().await.and_then(|peers| {
                                    let peer = peers
                                        .iter()
                                        .find(|peer| {
                                            peer.endpoint_id == connection.remote_id().to_string()
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
                    }
                    _ => Err(NetworkError::InvalidRequest),
                }
            }
            Ok(data) => match serde_json::from_slice::<BindRequest>(&data) {
                Ok(request) => {
                    let state = self.state.lock().await;
                    if state.pending.is_some()
                        || !state
                            .active
                            .as_ref()
                            .is_some_and(|active| Arc::ptr_eq(&active.session, session))
                    {
                        Err(NetworkError::NotReady)
                    } else {
                        self.pairings
                            .bind(connection.remote_id(), request)
                            .await
                            .map(|response| {
                                serde_json::to_value(response).expect("绑定响应可序列化")
                            })
                    }
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
        let state = self.state.lock().await;
        if state.pending.is_some()
            || !state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.session, session))
        {
            return Err(NetworkError::NotReady);
        }
        self.pairings
            .list()
            .await?
            .into_iter()
            .find(|peer| peer.endpoint_id == id.to_string())
            .ok_or(NetworkError::PeerNotBound)
    }

    pub(super) async fn peer_session(
        &self,
        id: iroh::EndpointId,
    ) -> Result<serde_json::Value, super::protocol::NetworkError> {
        use super::protocol::NetworkError;
        let state = self.state.lock().await;
        let active = state
            .active
            .as_ref()
            .filter(|_| state.pending.is_none())
            .ok_or(NetworkError::NotReady)?;
        let peer = self
            .pairings
            .list()
            .await?
            .into_iter()
            .find(|peer| peer.endpoint_id == id.to_string())
            .ok_or(NetworkError::PeerNotBound)?;
        let name = self
            .status
            .value_for("service", "name", true)
            .ok()
            .and_then(|v| v.value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "Loki 电脑".into());
        Ok(
            serde_json::json!({"api_version":"1","endpoint_id":active.endpoint_id,"status":"online","bound_at":peer.bound_at,"name":name,"device_type":"computer"}),
        )
    }

    pub fn new(
        config: ConfigService,
        registrar: Arc<dyn RelayRegistrar>,
        notification: Arc<NotificationService>,
        status: Arc<StatusService>,
        registration_timeout: Duration,
    ) -> Self {
        status
            .publish(
                "network",
                "my",
                serde_json::json!({"status": ConnectionStatus::Offline}),
            )
            .expect("固定状态路径有效");
        let pairings = Arc::new(PairingService::new(config.clone()));
        Self {
            remote: super::remote::Gateway::new(),
            config,
            registrar,
            notification,
            state: Mutex::new(State::default()),
            registration_timeout,
            status,
            connection_changed: Notify::new(),
            pairing_changed: Notify::new(),
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
                            "身份私钥已保存到 pass 密码库。\nEndpoint ID：{}",
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

    fn publish_phase(&self, state: &mut State, phase: ConnectionStatus) {
        if state.phase != phase {
            state.phase = phase;
            self.status
                .publish("network", "my", serde_json::json!({"status": phase}))
                .expect("固定状态路径有效");
        }
    }

    fn restore_active_phase(&self, state: &mut State, fallback: ConnectionStatus) {
        let phase = state
            .active
            .as_ref()
            .map(|active| active.session.status())
            .unwrap_or(fallback);
        self.publish_phase(state, phase);
    }

    async fn prepare(&self, state: &mut State, settings: RelaySettings) -> Result<(), String> {
        if state.pending.is_some() {
            return Err("网络配置正在提交，请稍后重试".to_owned());
        }
        let previous_status = state.phase;
        if settings.configured() {
            self.publish_phase(state, ConnectionStatus::Connecting);
        }
        let identity = match self.identity().await {
            Ok(identity) => identity,
            Err(error) => {
                self.restore_active_phase(
                    state,
                    if settings.configured() {
                        ConnectionStatus::Failed
                    } else {
                        ConnectionStatus::Offline
                    },
                );
                return Err(error);
            }
        };
        if !settings.configured() {
            state.pending = Some(PendingRegistration {
                candidate: None,
                previous_status,
            });
            return Ok(());
        }
        let endpoint_id = identity.public().to_string();
        if state.active.as_ref().is_some_and(|active| {
            active.settings == settings
                && active.endpoint_id == endpoint_id
                && active.session.status() == ConnectionStatus::Connected
        }) {
            self.restore_active_phase(state, ConnectionStatus::Connected);
            return Ok(());
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
            Ok(session) => {
                state.pending = Some(PendingRegistration {
                    candidate: Some(Registration {
                        settings: settings.clone(),
                        endpoint_id,
                        session,
                    }),
                    previous_status,
                });
                Ok(())
            }
            Err(error) => {
                let error = error.replace(&settings.token, "[已隐藏]");
                self.restore_active_phase(state, ConnectionStatus::Failed);
                self.notify("中转注册失败", error.clone()).await;
                Err(error)
            }
        }
    }

    async fn finish(&self, state: &mut State) {
        if let Some(pending) = state.pending.take() {
            self.pairings.invalidate().await;
            let old = std::mem::replace(&mut state.active, pending.candidate);
            self.restore_active_phase(state, ConnectionStatus::Offline);
            self.connection_changed.notify_one();
            self.pairing_changed.notify_one();
            if let Some(old) = old {
                if let Err(error) = old.session.close().await {
                    eprintln!("关闭原中转连接失败：{error}");
                }
            }
            if let Some(active) = &state.active {
                self.notify(
                    "中转注册成功",
                    format!(
                        "中转地址：{}\nEndpoint ID：{}",
                        active.settings.url, active.endpoint_id
                    ),
                )
                .await;
            }
        }
    }

    async fn discard_pending(&self, state: &mut State) -> Result<(), String> {
        if let Some(pending) = state.pending.take() {
            self.publish_phase(state, pending.previous_status);
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

    /// 由 main 显式运行，监听活动连接变化；状态中心只接收发布值。
    pub async fn monitor(&self) {
        loop {
            let changed = self.connection_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let active = {
                let state = self.state.lock().await;
                if state.pending.is_some() {
                    None
                } else {
                    state
                        .active
                        .as_ref()
                        .map(|active| Arc::clone(&active.session))
                }
            };
            if let Some(session) = active {
                let phase = session.status();
                self.publish_session_phase(&session, phase).await;
                if phase == ConnectionStatus::Disconnected {
                    changed.await;
                    continue;
                }
                tokio::select! {
                    _ = &mut changed => {},
                    result = session.status_changed(phase) => {
                        if let Ok(next) = result { self.publish_session_phase(&session, next).await; }
                    }
                }
            } else {
                changed.await;
            }
        }
    }

    async fn publish_session_phase(
        &self,
        session: &Arc<dyn RelaySession>,
        phase: ConnectionStatus,
    ) {
        let mut state = self.state.lock().await;
        if state.pending.is_none()
            && state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.session, session))
        {
            self.publish_phase(&mut state, phase);
        }
    }

    /// 由进程入口显式调用；失败不影响本机 HTTP 服务，配置提交后仍可重试。
    pub async fn start(&self) -> Result<(), String> {
        if let Err(error) = self.pairings.initialize().await {
            eprintln!("加载设备绑定失败：{error}");
        }
        let mut state = self.state.lock().await;
        // 配置提交已在处理新连接时，由该事务负责完成，不重放旧配置。
        if state.pending.is_some() {
            return Ok(());
        }
        let url = self.relay_url().await?;
        let token = relay_token(&self.config)
            .await
            .map_err(|error| error.to_string())?;
        self.prepare(&mut state, RelaySettings { url, token })
            .await?;
        self.finish(&mut state).await;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn config(&self) -> ConfigService {
        self.config.clone()
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        self.pairings.invalidate().await;
        self.publish_phase(&mut state, ConnectionStatus::Disconnected);
        self.connection_changed.notify_one();
        self.pairing_changed.notify_one();
        if let Some(pending) = state.pending.take() {
            if let Some(candidate) = pending.candidate {
                let _ = candidate.session.close().await;
            }
        }
        if let Some(active) = state.active.take() {
            let _ = active.session.close().await;
        }
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
            let mut state = self.state.lock().await;
            // 先导入旧令牌，避免换新后留下待迁移的旧值。
            relay_token(&self.config)
                .await
                .map_err(|error| error.to_string())?;
            let url = self.relay_url().await?;
            self.prepare(
                &mut state,
                RelaySettings {
                    url,
                    token: token.to_owned(),
                },
            )
            .await?;
            let password = relay_password();
            let saved = if token.trim().is_empty() {
                password.remove(&self.config).await
            } else {
                password.write(&self.config, token).await
            };
            if let Err(error) = saved {
                self.discard_pending(&mut state).await?;
                return Err(error.to_string());
            }
            self.finish(&mut state).await;
            Ok(())
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
            let mut state = self.state.lock().await;
            let token = relay_token(&self.config)
                .await
                .map_err(|error| error.to_string())?;
            self.prepare(&mut state, RelaySettings { url, token }).await
        })
    }

    fn committed(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network" && change.key == "relay_url" {
                self.finish(&mut *self.state.lock().await).await;
            }
            Ok(())
        })
    }

    fn rollback(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network" && change.key == "relay_url" {
                self.discard_pending(&mut *self.state.lock().await).await?;
            }
            Ok(())
        })
    }
}
