use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

pub const API_VERSION: &str = "1";
pub type StatusValues = BTreeMap<String, BTreeMap<String, Value>>;

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct StatusResponse {
    pub api_version: String,
    pub status: StatusValues,
    pub updated_at: Option<u64>,
}

impl crate::http::sse::SseEvent for StatusResponse {
    const EVENT: &'static str = "status";
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusModuleResponse {
    pub api_version: String,
    pub module_id: String,
    pub value: BTreeMap<String, Value>,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusValueResponse {
    pub api_version: String,
    pub module_id: String,
    pub key: String,
    pub value: Value,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusPathResponse {
    pub api_version: String,
    pub module_id: String,
    pub path: String,
    pub value: Value,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusMetaResponse {
    pub api_version: String,
    pub modules: Vec<super::model::StatusModuleMetadata>,
}
