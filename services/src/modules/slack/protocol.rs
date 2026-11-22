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
    Attachment,
    UserNotFound,
    Project,
    TeamMismatch,
    Thread,
    SendRecord,
    Authorization,
    Configuration,
    Session,
    Conflict,
    ReadPosition,
    ReadPositionStorage,
    UserToken,
    WebUserToken,
    WebCookie,
    WebSessionIncomplete,
    RateLimited(u64),
    Upstream,
    Unavailable,
    PasswordRead,
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
            Self::Attachment => (
                404,
                "slack-attachment-not-found",
                "找不到可读取的 Slack 附件。",
            ),
            Self::UserNotFound => (404, "slack-user-not-found", "找不到 Slack 用户。"),
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
            Self::ReadPositionStorage => (
                503,
                "slack-read-position-storage-error",
                "无法将 Slack 已读位置保存到本地缓存。",
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
            Self::DeliveryUnknown => (
                503,
                "slack-delivery-unknown",
                "发送结果未知，请刷新会话核对。",
            ),
        }
    }

    fn source(self) -> ErrorSource {
        match self {
            Self::Invalid | Self::Method => ErrorSource::Services,
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

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Author {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Message {
    pub ts: String,
    pub thread_ts: Option<String>,
    pub author: Option<Author>,
    pub text: String,
    pub reply_count: u64,
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_emoji: Option<String>,
    #[serde(default, skip_serializing)]
    pub icon_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Vec<Vec<String>>>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Attachment {
    pub title: String,
    pub mimetype: Option<String>,
    pub size: Option<u64>,
    pub url: Option<String>,
    pub text: Option<String>,
    pub image: bool,
    #[serde(skip)]
    pub image_url: Option<String>,
    #[serde(skip)]
    pub file_id: Option<String>,
    #[serde(skip)]
    pub filename: Option<String>,
    #[serde(skip)]
    pub download_url: Option<String>,
}

impl Attachment {
    fn from_slack(value: &Value, file: bool) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let field = |keys: &[&str]| {
            keys.iter()
                .filter_map(|key| value[*key].as_str())
                .find(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let mimetype = field(&["mimetype"]);
        let image_url = if file
            && mimetype
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
        {
            field(&["thumb_480", "thumb_360", "thumb_720", "url_private"])
        } else if !file {
            field(&["image_url", "thumb_url"])
        } else {
            None
        };
        Some(Self {
            title: field(&["title", "name", "fallback", "filetype"])
                .unwrap_or_else(|| "附件".into()),
            mimetype,
            size: value["size"].as_u64(),
            url: field(&["permalink", "title_link", "from_url"]),
            text: (!file).then(|| field(&["text", "pretext"])).flatten(),
            image: image_url
                .as_deref()
                .is_some_and(super::client::slack_image_url),
            image_url,
            file_id: file
                .then(|| field(&["id"]).filter(|id| valid_id(id)))
                .flatten(),
            filename: field(&["name"]),
            download_url: if file {
                field(&["url_private_download", "url_private"])
            } else {
                field(&["image_url", "thumb_url"])
            }
            .filter(|url| super::client::slack_image_url(url)),
        })
    }
}

fn table_text(cell: &Value, output: &mut String) {
    if let Some(text) = cell["text"].as_str() {
        output.push_str(text);
    } else if let Some(elements) = cell["elements"].as_array() {
        for element in elements {
            table_text(element, output);
        }
    } else if let Some(url) = cell["url"].as_str() {
        output.push_str(url);
    } else if let Some(id) = cell["user_id"].as_str() {
        output.push_str(&format!("@{id}"));
    }
}

fn table_rows(block: &Value) -> Option<Vec<Vec<String>>> {
    if block["type"] != "table" {
        return None;
    }
    Some(
        block["rows"]
            .as_array()?
            .iter()
            .filter_map(Value::as_array)
            .map(|row| {
                row.iter()
                    .map(|cell| {
                        let mut text = String::new();
                        table_text(cell, &mut text);
                        text
                    })
                    .collect()
            })
            .collect(),
    )
}

impl Message {
    pub fn mentions(&self, user: &str) -> bool {
        self.text.match_indices("<@").any(|(start, _)| {
            self.text[start + 2..]
                .strip_prefix(user)
                .is_some_and(|tail| {
                    tail.starts_with('>')
                        || tail
                            .strip_prefix('|')
                            .is_some_and(|label| label.contains('>'))
                })
        })
    }

    pub fn preview(&self) -> String {
        let text = self.text.trim();
        if text.is_empty() {
            return self
                .attachment
                .clone()
                .or_else(|| (!self.tables.is_empty()).then(|| "表格".into()))
                .unwrap_or_default();
        }
        text.chars().take(120).collect()
    }

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
        let files = value["files"].as_array().into_iter().flatten();
        let unfurls = value["attachments"].as_array().into_iter().flatten();
        let attachments: Vec<_> = files
            .filter_map(|file| Attachment::from_slack(file, true))
            .chain(
                unfurls
                    .clone()
                    .filter_map(|card| Attachment::from_slack(card, false)),
            )
            .collect();
        let attachment = attachments.first().map(|file| {
            format!(
                "附件 · {}",
                file.title.chars().take(100).collect::<String>()
            )
        });
        let tables = value["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(unfurls.flat_map(|card| card["blocks"].as_array().into_iter().flatten()))
            .filter_map(table_rows)
            .collect();
        Some(Self {
            ts: ts.into(),
            thread_ts: thread.map(str::to_owned),
            author,
            text: value["text"].as_str().unwrap_or_default().to_owned(),
            reply_count: value["reply_count"].as_u64().unwrap_or_default(),
            edited: value.get("edited").is_some(),
            icon_emoji: value["icons"]["emoji"]
                .as_str()
                .filter(|emoji| {
                    emoji.strip_prefix(':').and_then(|name| name.strip_suffix(':'))
                        .is_some_and(|name| {
                            !name.is_empty() && name.len() <= 64 && name.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+')
                            })
                        })
                })
                .map(str::to_owned),
            icon_url: ["image_48", "image_72", "image_36"]
                .into_iter()
                .filter_map(|size| value["icons"][size].as_str())
                .find(|url| super::client::message_icon_url(url))
                .map(str::to_owned),
            attachment,
            attachments,
            tables,
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
