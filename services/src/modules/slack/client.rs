use crate::process::Process;

use std::{collections::HashMap, sync::LazyLock, time::Duration};

use reqwest::{
    Client, Method, RequestBuilder, StatusCode,
    header::{ACCEPT_ENCODING, IF_RANGE, RANGE, RETRY_AFTER},
};
use serde_json::Value;
use tokio::{net::TcpStream, time::Instant};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::client::IntoClientRequest,
};
use url::Url;

use super::protocol::Error;

pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(super) fn attachment_mime(response: &reqwest::Response) -> &str {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or("application/octet-stream")
}

pub(super) fn slack_image_url(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.host_str().is_some_and(|host| {
                matches!(host, "files.slack.com" | "files-origin.slack.com")
                    || host.ends_with(".slack-edge.com")
            })
    })
}

fn slack_bot_icon_storage(value: &str) -> bool {
    Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.host_str() == Some("s3-us-west-2.amazonaws.com")
            && url.path().starts_with("/slack-files2/bot_icons/")
            && url.path().ends_with(".png")
    })
}

pub(super) fn message_icon_url(value: &str) -> bool {
    slack_image_url(value) || slack_bot_icon_storage(value)
}

pub(super) const WEB_TOKEN_PATTERN: &str = r"^xoxc-[A-Za-z0-9_-]{1,4091}$";
static WEB_TOKEN_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(WEB_TOKEN_PATTERN).expect("Slack 网页令牌正则有效"));

#[derive(Clone, serde::Deserialize)]
pub(super) struct WebSession {
    pub token: String,
    pub d_cookie: String,
}

impl WebSession {
    pub fn from_passwords(passwords: &[Option<String>; 2]) -> Result<Option<Self>, Error> {
        let [token, d_cookie] = passwords
            .each_ref()
            .map(|value| value.as_deref().map(str::trim));
        let (Some(token), Some(d_cookie)) = (token, d_cookie) else {
            return Ok(None);
        };
        Self {
            token: token.into(),
            d_cookie: d_cookie.into(),
        }
        .validate()
        .map(Some)
    }

    fn validate(mut self) -> Result<Self, Error> {
        if let Some(value) = self.d_cookie.strip_prefix("d=") {
            self.d_cookie = value.to_owned();
        }
        let valid_cookie = |cookie: &str| {
            !cookie.is_empty()
                && cookie.len() <= 8192
                && cookie.bytes().all(
                    |byte| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e),
                )
        };
        if !WEB_TOKEN_REGEX.is_match(&self.token) {
            return Err(Error::WebUserToken);
        }
        if !valid_cookie(&self.d_cookie) {
            return Err(Error::WebCookie);
        }
        Ok(self)
    }
}

#[derive(Clone)]
pub(super) struct SlackClient {
    http: Client,
    attachments: Client,
    api: String,
    limits: Process<HashMap<String, Instant>>,
}

impl SlackClient {
    pub async fn image(
        &self,
        url: &str,
        token: &str,
        web: Option<&WebSession>,
    ) -> Result<(String, Vec<u8>), Error> {
        self.download(url, token, web, 5 * 1024 * 1024, false).await
    }

    pub async fn transfer_original(
        &self,
        url: &str,
        token: &str,
        web: Option<&WebSession>,
        range: Option<(u64, &str)>,
    ) -> Result<reqwest::Response, Error> {
        let mut request = self
            .download_request(&self.attachments, url, token, web, false)?
            .header(ACCEPT_ENCODING, "identity");
        if let Some((offset, etag)) = range {
            request = request
                .header(RANGE, format!("bytes={offset}-"))
                .header(IF_RANGE, etag);
        }
        request.send().await.map_err(|_| Error::Unavailable)
    }

    pub async fn avatar(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.download(url, "", None, 512 * 1024, true)
            .await
            .map(|(_, bytes)| bytes)
    }

    async fn download(
        &self,
        url: &str,
        token: &str,
        web: Option<&WebSession>,
        limit: usize,
        avatar: bool,
    ) -> Result<(String, Vec<u8>), Error> {
        let (mime, mut response) = self.download_response(url, token, web, avatar).await?;
        if super::disk::mime_extension(&mime)
            .and_then(super::disk::preview_mime)
            .is_none()
            || response
                .content_length()
                .is_some_and(|size| size > limit as u64)
        {
            return Err(Error::Attachment);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
            if chunk.len() > limit - bytes.len() {
                return Err(Error::Attachment);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((mime, bytes))
    }

    async fn download_response(
        &self,
        url: &str,
        token: &str,
        web: Option<&WebSession>,
        avatar: bool,
    ) -> Result<(String, reqwest::Response), Error> {
        let response = self
            .download_request(&self.http, url, token, web, avatar)?
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Attachment);
        }
        Ok((attachment_mime(&response).to_owned(), response))
    }

    fn download_request(
        &self,
        client: &Client,
        url: &str,
        token: &str,
        web: Option<&WebSession>,
        avatar: bool,
    ) -> Result<RequestBuilder, Error> {
        let approved_avatar = avatar
            && Url::parse(url).ok().is_some_and(|url| {
                url.scheme() == "https"
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.port().is_none()
                    && url.host_str().is_some_and(|host| {
                        host == "slack.com"
                            || host.ends_with(".slack.com")
                            || host == "gravatar.com"
                            || host.ends_with(".gravatar.com")
                    })
            });
        if !slack_image_url(url) && !approved_avatar && !(avatar && slack_bot_icon_storage(url)) {
            return Err(Error::Attachment);
        }
        let url = Url::parse(url).map_err(|_| Error::Attachment)?;
        let private = matches!(
            url.host_str(),
            Some("files.slack.com" | "files-origin.slack.com")
        );
        #[cfg(test)]
        let url = Url::parse(&self.api)
            .ok()
            .filter(|api| api.scheme() == "http" && api.host_str() == Some("127.0.0.1"))
            .map_or(url.clone(), |mut api| {
                api.set_path(url.path());
                api.set_query(url.query());
                api
            });
        let mut request = client.get(url);
        if private && !avatar {
            request = request.bearer_auth(web.map_or(token, |web| &web.token));
            if let Some(web) = web {
                request = request.header(reqwest::header::COOKIE, format!("d={}", web.d_cookie));
            }
        }
        Ok(request)
    }

    pub fn new(api: &str) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(8))
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Slack HTTP 客户端配置有效"),
            attachments: Client::builder()
                .connect_timeout(Duration::from_secs(8))
                .read_timeout(Duration::from_secs(30))
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Slack 附件 HTTP 客户端配置有效"),
            api: api.trim_end_matches('/').to_owned(),
            limits: Process::start(HashMap::new()),
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
        let method_name = name.to_owned();
        if let Some(delay) = self
            .limits
            .call(move |limits| {
                limits
                    .get(&method_name)?
                    .checked_duration_since(Instant::now())
            })
            .await
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
            let name = name.to_owned();
            self.limits.post(move |limits| {
                limits.insert(name, Instant::now() + Duration::from_secs(seconds));
            });
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
            Some("user_not_found" | "users_not_found" | "bot_not_found") => Error::UserNotFound,
            Some("thread_not_found") => Error::Thread,
            Some("invalid_arguments" | "msg_too_long" | "no_text") => Error::Invalid,
            _ => Error::Upstream,
        })
    }

    pub async fn connect(&self, token: &str, web: Option<&WebSession>) -> Result<Socket, Error> {
        let value = self
            .call(
                "rtm.connect",
                Method::POST,
                Some(web.map_or(token, |session| &session.token)),
                &[],
                None,
                web.map(|session| session.d_cookie.as_str()),
            )
            .await?;
        let url = Url::parse(value["url"].as_str().ok_or(Error::Upstream)?)
            .map_err(|_| Error::Upstream)?;
        if !self.trusted_socket(&url) || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Upstream);
        }
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| Error::Upstream)?;
        if let Some(session) = web {
            let value = format!("d={}", session.d_cookie)
                .parse()
                .map_err(|_| Error::WebCookie)?;
            request.headers_mut().insert("cookie", value);
        }
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(request))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(|error| match error {
                tokio_tungstenite::tungstenite::Error::Http(response)
                    if matches!(response.status().as_u16(), 401 | 403) =>
                {
                    Error::Authorization
                }
                _ => Error::Unavailable,
            })?;
        Ok(socket)
    }

    fn trusted_socket(&self, url: &Url) -> bool {
        #[cfg(test)]
        if self.api.starts_with("http://127.0.0.1:")
            && url.scheme() == "ws"
            && url.host_str() == Some("127.0.0.1")
        {
            return true;
        }
        url.scheme() == "wss"
            && url
                .host_str()
                .is_some_and(|host| host == "slack.com" || host.ends_with(".slack.com"))
    }
}
