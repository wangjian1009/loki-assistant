use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{ConfigModuleMetadata, ConfigValue};

#[derive(Clone, Debug, Serialize)]
pub struct MetaResponse {
    pub modules: Vec<ConfigModuleMetadata>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleDataResponse {
    pub module_id: String,
    pub values: BTreeMap<String, ConfigValue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValueResponse {
    pub module_id: String,
    pub key: String,
    #[serde(flatten)]
    pub data: ConfigValue,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigErrorResponse {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ValidateRequest {
    pub module_id: String,
    pub key: String,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidateResponse {
    pub module_id: String,
    pub key: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ConfigErrorResponse>,
}
