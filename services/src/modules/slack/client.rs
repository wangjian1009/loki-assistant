use std::{collections::HashMap, sync::Arc, time::Duration};

use reqwest::{Client, Method, StatusCode, header::RETRY_AFTER};
use serde_json::Value;
use tokio::{net::TcpStream, sync::Mutex, time::Instant};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use url::Url;

use super::protocol::Error;

pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WebSession {
    pub token: String,
    pub d_cookie: String,
}

impl WebSession {
    pub fn parse(secret: &str) -> Result<Self, Error> {
        let mut session: Self = serde_json::from_str(secret).map_err(|_| Error::UserToken)?;
        if let Some(value) = session.d_cookie.strip_prefix("d=") {
            session.d_cookie = value.to_owned();
        }
        if !session.token.starts_with("xoxc-")
            || session.token.len() <= 5
            || session.token.len() > 4096
            || !session
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || session.d_cookie.is_empty()
            || session.d_cookie.len() > 8192
            || !session.d_cookie.bytes().all(
                |byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e),
            )
        {
            return Err(Error::UserToken);
        }
        Ok(session)
    }
}

#[derive(Clone)]
pub(super) struct SlackClient {
    http: Client,
    api: String,
    limits: Arc<Mutex<HashMap<String, Instant>>>,
}

impl SlackClient {
    pub fn new(api: &str) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(8))
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Slack HTTP 客户端配置有效"),
            api: api.trim_end_matches('/').to_owned(),
            limits: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get(
        &self,
        method: &str,
        token: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, Error> {
        self.call(method, Method::GET, Some(token), params, None, None)
            .await
    }

    pub async fn post(&self, method: &str, token: &str, body: &Value) -> Result<Value, Error> {
        self.call(method, Method::POST, Some(token), &[], Some(body), None)
            .await
    }

    pub async fn test_identity(&self, token: &str) -> Result<Value, Error> {
        self.call("auth.test", Method::POST, Some(token), &[], None, None)
            .await
    }

    pub async fn web_call(
        &self,
        method: &str,
        session: &WebSession,
        params: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, Error> {
        let verb = if body.is_some() || method == "auth.test" {
            Method::POST
        } else {
            Method::GET
        };
        self.call(
            method,
            verb,
            Some(&session.token),
            params,
            body,
            Some(&session.d_cookie),
        )
        .await
    }

    async fn call(
        &self,
        name: &str,
        method: Method,
        token: Option<&str>,
        params: &[(&str, &str)],
        body: Option<&Value>,
        cookie: Option<&str>,
    ) -> Result<Value, Error> {
        if let Some(until) = self.limits.lock().await.get(name)
            && let Some(delay) = until.checked_duration_since(Instant::now())
        {
            return Err(Error::RateLimited(delay.as_secs().saturating_add(1)));
        }
        let mut request = self
            .http
            .request(method.clone(), format!("{}/{name}", self.api));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(cookie) = cookie {
            request = request.header(reqwest::header::COOKIE, format!("d={cookie}"));
        }
        if let Some(body) = body {
            request = request.json(body);
        } else if method == Method::POST {
            request = request.form(params);
        } else {
            request = request.query(params);
        }
        let response = request.send().await.map_err(|_| Error::Unavailable)?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            let seconds = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(30)
                .clamp(1, 86_400);
            self.limits
                .lock()
                .await
                .insert(name.into(), Instant::now() + Duration::from_secs(seconds));
            return Err(Error::RateLimited(seconds));
        }
        if !response.status().is_success() {
            return Err(Error::Upstream);
        }
        let value: Value = response.json().await.map_err(|_| Error::Upstream)?;
        if value["ok"] == true {
            return Ok(value);
        }
        Err(match value["error"].as_str() {
            Some("missing_scope" | "no_permission" | "restricted_action" | "not_in_channel") => {
                Error::Permission
            }
            Some(
                "invalid_auth" | "token_expired" | "token_revoked" | "account_inactive"
                | "not_authed",
            ) => Error::Authorization,
            Some("channel_not_found") => Error::Conversation,
            Some("thread_not_found") => Error::Thread,
            Some("invalid_arguments" | "msg_too_long" | "no_text") => Error::Invalid,
            _ => Error::Upstream,
        })
    }

    async fn socket_url(&self, token: &str) -> Result<Url, Error> {
        let value = self
            .call(
                "apps.connections.open",
                Method::POST,
                Some(token),
                &[],
                None,
                None,
            )
            .await?;
        let url = Url::parse(value["url"].as_str().ok_or(Error::Upstream)?)
            .map_err(|_| Error::Upstream)?;
        if url.scheme() == "wss" {
            return Ok(url);
        }
        #[cfg(test)]
        if self.api.starts_with("http://127.0.0.1:")
            && url.scheme() == "ws"
            && url.host_str() == Some("127.0.0.1")
        {
            return Ok(url);
        }
        Err(Error::Upstream)
    }

    pub async fn connect(&self, token: &str) -> Result<Socket, Error> {
        let url = self.socket_url(token).await?;
        let (socket, _) =
            tokio::time::timeout(Duration::from_secs(10), connect_async(url.as_str()))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(|_| Error::Unavailable)?;
        Ok(socket)
    }
}
