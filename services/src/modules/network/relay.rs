use std::{fmt, future::Future, pin::Pin, sync::Arc};

use iroh::{Endpoint, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher, endpoint::presets};

pub type RelayFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait RelaySession: fmt::Debug + Send + Sync {
    fn close(&self) -> RelayFuture<'_, ()>;
}

pub trait RelayRegistrar: fmt::Debug + Send + Sync {
    fn register(&self, identity: SecretKey, url: String) -> RelayFuture<'_, Arc<dyn RelaySession>>;
}

pub struct IrohRelayRegistrar {
    token: Option<String>,
}

impl fmt::Debug for IrohRelayRegistrar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IrohRelayRegistrar")
            .finish_non_exhaustive()
    }
}

impl IrohRelayRegistrar {
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    fn redact(&self, message: String) -> String {
        match self.token.as_deref().filter(|token| !token.is_empty()) {
            Some(token) => message.replace(token, "[已隐藏]"),
            None => message,
        }
    }
}

#[derive(Debug)]
struct IrohSession(Endpoint);

impl RelaySession for IrohSession {
    fn close(&self) -> RelayFuture<'_, ()> {
        Box::pin(async {
            self.0.close().await;
            Ok(())
        })
    }
}

impl RelayRegistrar for IrohRelayRegistrar {
    fn register(&self, identity: SecretKey, url: String) -> RelayFuture<'_, Arc<dyn RelaySession>> {
        Box::pin(async move {
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
            if let Some(token) = &self.token {
                relays = relays.with_auth_token(token.clone());
            }
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(identity)
                .relay_mode(RelayMode::Custom(relays))
                .clear_ip_transports()
                .bind()
                .await
                .map_err(|error| self.redact(format!("无法创建网络端点：{error}")))?;
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
                    let message = self.redact(format!("中转服务器拒绝认证：{reason}"));
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
