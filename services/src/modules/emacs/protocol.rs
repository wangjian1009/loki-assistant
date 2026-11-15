use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: &str = "1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalRequest {
    pub script: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalResponse {
    pub api_version: String,
    /// Lisp 打印表示，保留 nil、符号、列表等类型的原始语义。
    pub result: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Buffer {
    pub name: String,
    pub file: Option<String>,
    pub modified: bool,
    pub major_mode: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BuffersResponse {
    pub api_version: String,
    pub buffers: Vec<Buffer>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Notification {
    #[serde(default = "default_notification_module")]
    pub module_id: String,
    pub title: String,
    pub icon: Option<String>,
    pub buffer: String,
    #[serde(default)]
    pub payload: Value,
}

fn default_notification_module() -> String {
    "emacs".to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NotificationResponse {
    pub api_version: String,
    pub module_id: String,
    pub result: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ErrorResponse {
    pub api_version: String,
    pub code: String,
    pub message: String,
}
