use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "1";

/// 业务模块提供通知内容和本机服务回调路径；平台跳转 URL 由通知模块生成。
#[derive(Clone, Debug)]
pub struct NotificationRequest {
    pub title: String,
    pub body: Option<String>,
    pub app: Option<String>,
    pub callback_url: Option<String>,
}

/// 应用标识对应 center 内置资源名称，不接受路径或 URL。
pub(crate) fn is_valid_app(app: &str) -> bool {
    !app.is_empty()
        && app.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotificationReceipt {
    Submitted {},
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationStatus {
    Forwarded,
    Submitted,
    Failed,
    Unconfirmed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NotificationStatusResponse {
    pub api_version: String,
    pub notification_id: String,
    pub status: NotificationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub api_version: String,
    pub code: String,
    pub message: String,
}
