use axum::{
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::http::error::{ErrorSource, response};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Error {
    Invalid,
    Cursor,
    Permission,
    Conversation,
    Project,
    TeamMismatch,
    Thread,
    SendRecord,
    Authorization,
    Configuration,
    Session,
    Conflict,
    ReadPosition,
    UserToken,
    WebUserToken,
    WebCookie,
    WebSessionIncomplete,
    RateLimited(u64),
    Upstream,
    Unavailable,
    Credentials,
    PasswordRead,
    Storage,
    DeliveryUnknown,
    Method,
}

impl Error {
    fn info(self) -> (u16, &'static str, &'static str) {
        match self {
            Self::Invalid => (400, "invalid-request", "Slack 请求参数无效。"),
            Self::Cursor => (
                400,
                "slack-invalid-cursor",
                "分页游标已失效，请刷新第一页。",
            ),
            Self::Permission => (403, "slack-permission-denied", "Slack 应用缺少所需权限。"),
            Self::Conversation => (404, "slack-conversation-not-found", "找不到 Slack 会话。"),
            Self::Project => (404, "slack-project-not-found", "请选择有效的 Slack 项目。"),
            Self::TeamMismatch => (
                409,
                "slack-team-mismatch",
                "实际授权的 Slack Team 与项目配置不匹配。",
            ),
            Self::Thread => (404, "slack-thread-not-found", "找不到 Slack 线程。"),
            Self::SendRecord => (
                404,
                "slack-send-request-not-found",
                "找不到 Slack 发送记录。",
            ),
            Self::Method => (405, "method-not-allowed", "此 Slack 接口不支持该方法。"),
            Self::Authorization => (
                409,
                "slack-authorization-required",
                "Slack 拒绝了当前凭据，请检查或更新。",
            ),
            Self::Configuration => (
                409,
                "slack-not-configured",
                "请配置 Slack 工作区及其用户凭据。",
            ),
            Self::Session => (
                409,
                "slack-session-changed",
                "Slack 会话已变更，请刷新状态。",
            ),
            Self::Conflict => (
                409,
                "slack-request-id-conflict",
                "此发送标识已用于另一条消息。",
            ),
            Self::ReadPosition => (
                422,
                "slack-invalid-read-position",
                "此消息不属于指定的时间线。",
            ),
            Self::UserToken => (
                422,
                "slack-invalid-user-token",
                "请输入 Slack 用户令牌（xoxp- 开头）。",
            ),
            Self::WebUserToken => (
                422,
                "slack-invalid-web-user-token",
                "请在「Slack 用户令牌」填写以 xoxc- 开头的完整网页会话令牌。",
            ),
            Self::WebCookie => (
                422,
                "slack-invalid-web-cookie",
                "Cookie d 格式无效。请只复制名称为 d 的条目在「值」列中的原始编码值。",
            ),
            Self::WebSessionIncomplete => (
                422,
                "slack-web-session-incomplete",
                "请补齐 Slack 网页会话的用户令牌和 Cookie d。",
            ),
            Self::RateLimited(_) => (429, "slack-rate-limited", "Slack 暂时限制请求频率。"),
            Self::Upstream => (502, "slack-upstream-error", "Slack 未能完成请求。"),
            Self::Unavailable => (503, "slack-data-unavailable", "暂时无法读取 Slack 数据。"),
            Self::PasswordRead => (
                503,
                "slack-password-unavailable",
                "无法读取项目的 pass 条目，请检查密码库设置。",
            ),
            Self::Credentials => (
                503,
                "slack-credentials-unavailable",
                "暂时无法访问 Slack 本机凭据。",
            ),
            Self::Storage => (
                503,
                "slack-storage-unavailable",
                "暂时无法读写 Slack 本机存储。",
            ),
            Self::DeliveryUnknown => (
                503,
                "slack-delivery-unknown",
                "发送结果未知，请刷新会话核对。",
            ),
        }
    }

    fn source(self) -> ErrorSource {
        match self {
            Self::Invalid | Self::Method | Self::Storage => ErrorSource::Services,
            _ => ErrorSource::Business,
        }
    }

    pub fn object(self) -> Value {
        let (_, code, message) = self.info();
        json!({"source":self.source(), "code":code, "message":message})
    }

    pub fn retry_after(self) -> Option<u64> {
        match self {
            Self::RateLimited(seconds) => Some(seconds),
            _ => None,
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = self.info();
        let mut result = response(
            StatusCode::from_u16(status).unwrap(),
            self.source(),
            code,
            message,
        );
        if let Some(seconds) = self.retry_after() {
            result
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        result
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Identity {
    pub session_id: Uuid,
    pub team_id: String,
    pub user_id: String,
    pub team_name: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Author {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Message {
    pub ts: String,
    pub thread_ts: Option<String>,
    pub author: Option<Author>,
    pub text: String,
    pub reply_count: u64,
    pub edited: bool,
}

impl Message {
    pub fn from_slack(value: &Value, root: Option<&str>) -> Option<Self> {
        let ts = value.get("ts")?.as_str()?;
        timestamp(ts)?;
        let thread = value["thread_ts"]
            .as_str()
            .or(root)
            .filter(|thread| *thread != ts && timestamp(thread).is_some());
        let author = value["user"]
            .as_str()
            .or(value["bot_id"].as_str())
            .map(|id| Author {
                id: id.into(),
                name: value["user_profile"]["display_name"]
                    .as_str()
                    .or(value["bot_profile"]["name"].as_str())
                    .or(value["username"].as_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id)
                    .into(),
            });
        Some(Self {
            ts: ts.into(),
            thread_ts: thread.map(str::to_owned),
            author,
            text: value["text"].as_str().unwrap_or_default().to_owned(),
            reply_count: value["reply_count"].as_u64().unwrap_or_default(),
            edited: value.get("edited").is_some(),
        })
    }
}

pub(super) fn timestamp(value: &str) -> Option<(u64, u32)> {
    let (seconds, fraction) = value.split_once('.')?;
    if seconds.is_empty()
        || fraction.is_empty()
        || fraction.len() > 9
        || !seconds
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((
        seconds.parse().ok()?,
        fraction.parse::<u32>().ok()? * 10_u32.pow(9 - fraction.len() as u32),
    ))
}

pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Page {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub unread: Option<bool>,
    pub kind: Option<String>,
}

impl Page {
    pub fn limit(&self) -> Result<usize, Error> {
        let limit = self.limit.unwrap_or(50);
        if (1..=100).contains(&limit) {
            Ok(limit)
        } else {
            Err(Error::Invalid)
        }
    }
}
