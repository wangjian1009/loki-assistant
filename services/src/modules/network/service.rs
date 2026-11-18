use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, Notify},
    time::timeout,
};

use super::{
    identity::IdentityStore,
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
    fn from_values(values: &BTreeMap<String, serde_json::Value>) -> Result<Self, String> {
        let read = |key: &str| {
            values
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("网络配置 {key} 必须是字符串"))
        };
        Ok(Self {
            url: read("relay_url")?,
            token: read("relay_token")?,
        })
    }

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
    identity: Result<IdentityStore, String>,
    registrar: Arc<dyn RelayRegistrar>,
    notification: Arc<NotificationService>,
    state: Mutex<State>,
    registration_timeout: Duration,
    status: Arc<StatusService>,
    connection_changed: Notify,
}

impl NetworkService {
    pub fn new(
        identity: Result<IdentityStore, String>,
        registrar: Arc<dyn RelayRegistrar>,
        notification: Arc<NotificationService>,
        status: Arc<StatusService>,
        registration_timeout: Duration,
    ) -> Self {
        status
            .publish(
                "network",
                "my",
                serde_json::json!({"status": ConnectionStatus::Disconnected}),
            )
            .expect("固定状态路径有效");
        Self {
            identity,
            registrar,
            notification,
            state: Mutex::new(State::default()),
            registration_timeout,
            status,
            connection_changed: Notify::new(),
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
                "{title}：通知已转交，notification_id={}",
                receipt.notification_id
            ),
            Err(error) => eprintln!("{title}，通知转交失败：{error}"),
        }
    }

    async fn identity(&self) -> Result<iroh::SecretKey, String> {
        let result = match &self.identity {
            Ok(store) => store.ensure().await,
            Err(message) => Err(message.clone()),
        };
        match result {
            Ok((identity, created)) => {
                if created {
                    self.notify(
                        "已生成本地身份",
                        format!(
                            "身份已保存到 ~/.assistant/identity.json。\nEndpoint ID：{}",
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
                        ConnectionStatus::Disconnected
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
            let old = std::mem::replace(&mut state.active, pending.candidate);
            self.restore_active_phase(state, ConnectionStatus::Disconnected);
            self.connection_changed.notify_one();
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
    pub async fn start(&self, config: Arc<ConfigService>) -> Result<(), String> {
        let mut state = self.state.lock().await;
        // 配置提交已在处理新连接时，由该事务负责完成，不重放旧配置。
        if state.pending.is_some() {
            return Ok(());
        }
        let values = config
            .module_values("network")
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(key, value)| (key, value.value))
            .collect();
        self.prepare(&mut state, RelaySettings::from_values(&values)?)
            .await?;
        self.finish(&mut state).await;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        self.publish_phase(&mut state, ConnectionStatus::Disconnected);
        self.connection_changed.notify_one();
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
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id != "network"
                || !matches!(change.key.as_str(), "relay_url" | "relay_token")
            {
                return Ok(());
            }
            let settings = RelaySettings::from_values(&change.module_values)?;
            let mut state = self.state.lock().await;
            self.prepare(&mut state, settings).await
        })
    }

    fn committed(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network"
                && matches!(change.key.as_str(), "relay_url" | "relay_token")
            {
                self.finish(&mut *self.state.lock().await).await;
            }
            Ok(())
        })
    }

    fn rollback(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id == "network"
                && matches!(change.key.as_str(), "relay_url" | "relay_token")
            {
                let mut state = self.state.lock().await;
                if let Some(pending) = state.pending.take() {
                    self.publish_phase(&mut state, pending.previous_status);
                    self.connection_changed.notify_one();
                    if let Some(candidate) = pending.candidate {
                        candidate.session.close().await?;
                    }
                    self.notify(
                        "中转配置保存失败",
                        "配置未能落盘，已撤销本次注册并保留原连接。".to_owned(),
                    )
                    .await;
                }
            }
            Ok(())
        })
    }
}
