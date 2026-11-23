use serde::{Deserialize, Serialize};

pub(super) const API_VERSION: &str = "1";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertLevel {
    Info,
    #[default]
    Warning,
    Error,
}

/// 调用方提供的完整告警内容。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AlertInput {
    pub app: Option<String>,
    pub title: String,
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    #[serde(default)]
    pub level: AlertLevel,
}

/// 状态中心发布的单条告警，时间以 Unix 秒表示。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Alert {
    pub id: String,
    pub app: Option<String>,
    pub title: String,
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_url: Option<String>,
    pub level: AlertLevel,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct PublishAlertResponse {
    pub api_version: &'static str,
    pub alert: Alert,
}
