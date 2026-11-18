use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

pub const API_VERSION: &str = "1";
pub type StatusValues = BTreeMap<String, BTreeMap<String, Value>>;

#[derive(Clone, Debug, Serialize)]
pub struct StatusResponse {
    pub api_version: String,
    pub status: StatusValues,
    pub updated_at: Option<u64>,
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
pub struct StatusMetaResponse {
    pub api_version: String,
    pub modules: Vec<super::model::StatusModuleMetadata>,
}
