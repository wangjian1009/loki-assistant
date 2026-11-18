use std::{sync::Arc, time::Duration};

use tokio::{sync::Mutex, time::timeout};

use super::{
    identity::IdentityStore,
    relay::{RelayRegistrar, RelaySession},
};
use crate::modules::{
    config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler, ConfigService},
    notification::{NotificationService, protocol::NotificationRequest},
};

#[derive(Debug)]
struct Registration {
    url: String,
    endpoint_id: String,
    session: Arc<dyn RelaySession>,
}

#[derive(Debug, Default)]
struct State {
    active: Option<Registration>,
    pending: Option<Option<Registration>>,
}

/// 持有当前中转连接；新地址完成注册并落盘后才替换原连接。
#[derive(Debug)]
pub struct NetworkService {
    identity: Result<IdentityStore, String>,
    registrar: Arc<dyn RelayRegistrar>,
    notification: Arc<NotificationService>,
    state: Mutex<State>,
    registration_timeout: Duration,
}

impl NetworkService {
    pub fn new(
        identity: Result<IdentityStore, String>,
        registrar: Arc<dyn RelayRegistrar>,
        notification: Arc<NotificationService>,
        registration_timeout: Duration,
    ) -> Self {
        Self {
            identity,
            registrar,
            notification,
            state: Mutex::new(State::default()),
            registration_timeout,
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

    async fn prepare(&self, state: &mut State, url: &str) -> Result<(), String> {
        let identity = self.identity().await?;
        if state.pending.is_some() {
            return Err("网络配置正在提交，请稍后重试".to_owned());
        }
        if url.is_empty() {
            if state.active.is_some() {
                state.pending = Some(None);
            }
            return Ok(());
        }
        let endpoint_id = identity.public().to_string();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.url == url && active.endpoint_id == endpoint_id)
        {
            return Ok(());
        }
        let result = timeout(
            self.registration_timeout,
            self.registrar.register(identity, url.to_owned()),
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
                state.pending = Some(Some(Registration {
                    url: url.to_owned(),
                    endpoint_id,
                    session,
                }));
                Ok(())
            }
            Err(error) => {
                self.notify("中转注册失败", error.clone()).await;
                Err(error)
            }
        }
    }

    async fn finish(&self, state: &mut State) {
        if let Some(candidate) = state.pending.take() {
            let old = std::mem::replace(&mut state.active, candidate);
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
                        active.url, active.endpoint_id
                    ),
                )
                .await;
            }
        }
    }

    /// 由进程入口显式调用；失败不影响本机 HTTP 服务，配置提交后仍可重试。
    pub async fn start(&self, config: Arc<ConfigService>) -> Result<(), String> {
        let mut state = self.state.lock().await;
        // 配置提交已在处理新连接时，由该事务负责完成，不重放旧配置。
        if state.pending.is_some() {
            return Ok(());
        }
        let url = config
            .value("network", "relay_url")
            .await
            .map_err(|error| error.to_string())?
            .value
            .as_str()
            .ok_or("中转地址必须是字符串")?
            .to_owned();
        self.prepare(&mut state, &url).await?;
        self.finish(&mut state).await;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        if let Some(Some(pending)) = state.pending.take() {
            let _ = pending.session.close().await;
        }
        if let Some(active) = state.active.take() {
            let _ = active.session.close().await;
        }
    }
}

impl ConfigChangeHandler for NetworkService {
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id != "network" || change.key != "relay_url" {
                return Ok(());
            }
            let url = change.value.as_str().ok_or("中转地址必须是字符串")?;
            let mut state = self.state.lock().await;
            self.prepare(&mut state, url).await
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
                let mut state = self.state.lock().await;
                if let Some(candidate) = state.pending.take() {
                    if let Some(candidate) = candidate {
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
