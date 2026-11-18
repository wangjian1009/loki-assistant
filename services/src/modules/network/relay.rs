use std::{fmt, future::Future, pin::Pin, sync::Arc};

use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher, endpoint::presets};

use super::protocol::ConnectionStatus;

pub type RelayFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait RelaySession: fmt::Debug + Send + Sync {
    fn status(&self) -> ConnectionStatus;
    fn status_changed(&self, _current: ConnectionStatus) -> RelayFuture<'_, ConnectionStatus> {
        Box::pin(std::future::pending())
    }
    fn close(&self) -> RelayFuture<'_, ()>;
}

pub trait RelayRegistrar: fmt::Debug + Send + Sync {
    fn register(
        &self,
        identity: SecretKey,
        url: String,
        token: String,
    ) -> RelayFuture<'_, Arc<dyn RelaySession>>;
}

#[derive(Debug, Default)]
pub struct IrohRelayRegistrar;

impl IrohRelayRegistrar {
    pub fn new() -> Self {
        Self
    }

    fn redact(message: String, token: &str) -> String {
        if token.is_empty() {
            message
        } else {
            message.replace(token, "[已隐藏]")
        }
    }
}

#[derive(Debug)]
struct IrohSession(Endpoint);

impl RelaySession for IrohSession {
    fn status(&self) -> ConnectionStatus {
        if self.0.is_closed() {
            return ConnectionStatus::Disconnected;
        }
        let relays = self.0.home_relay_status().get();
        if relays.iter().any(|relay| relay.is_connected()) {
            ConnectionStatus::Connected
        } else if relays.iter().any(|relay| relay.last_error().is_some()) {
            ConnectionStatus::Failed
        } else {
            ConnectionStatus::Connecting
        }
    }

    fn status_changed(&self, current: ConnectionStatus) -> RelayFuture<'_, ConnectionStatus> {
        Box::pin(async move {
            let mut watcher = self.0.home_relay_status();
            loop {
                let next = self.status();
                if next != current {
                    return Ok(next);
                }
                tokio::select! {
                    _ = self.0.closed() => return Ok(ConnectionStatus::Disconnected),
                    result = watcher.updated() => {
                        if result.is_err() { return Ok(ConnectionStatus::Disconnected); }
                    }
                }
            }
        })
    }

    fn close(&self) -> RelayFuture<'_, ()> {
        Box::pin(async {
            self.0.close().await;
            Ok(())
        })
    }
}

impl RelayRegistrar for IrohRelayRegistrar {
    fn register(
        &self,
        identity: SecretKey,
        url: String,
        token: String,
    ) -> RelayFuture<'_, Arc<dyn RelaySession>> {
        Box::pin(async move {
            if token.trim().is_empty() {
                return Err("中转令牌不能为空".to_owned());
            }
            let relay: RelayUrl = url
                .parse()
                .map_err(|_| "中转地址不是有效的 URL".to_owned())?;
            let parsed = relay
                .as_str()
                .parse::<url::Url>()
                .map_err(|_| "中转地址无效".to_owned())?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err("中转地址须为 HTTP(S) URL，不能包含账号、密码、查询或片段".to_owned());
            }
            let mut relays: RelayMap = [relay].into_iter().collect();
            relays = relays.with_auth_token(token.clone());
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(identity)
                .relay_mode(RelayMode::Custom(relays))
                .clear_ip_transports()
                .bind()
                .await
                .map_err(|error| Self::redact(format!("无法创建网络端点：{error}"), &token))?;
            let mut watcher = endpoint.home_relay_status();
            loop {
                let statuses = watcher.get();
                if statuses.iter().any(|status| status.is_connected()) {
                    return Ok(Arc::new(IrohSession(endpoint)) as Arc<dyn RelaySession>);
                }
                if let Some(reason) = statuses
                    .iter()
                    .find_map(|status| status.auth_denied_reason())
                {
                    let message = Self::redact(format!("中转服务器拒绝认证：{reason}"), &token);
                    endpoint.close().await;
                    return Err(message);
                }
                if watcher.updated().await.is_err() {
                    endpoint.close().await;
                    return Err("中转连接已关闭".to_owned());
                }
            }
        })
    }
}
