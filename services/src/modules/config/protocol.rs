use serde::Serialize;
use serde_json::{Map, Value};

use super::model::ConfigModuleMetadata;

#[derive(Clone, Debug, Serialize)]
pub struct MetaResponse {
    pub modules: Vec<ConfigModuleMetadata>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleDataResponse {
    pub module_id: String,
    pub values: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValueResponse {
    pub module_id: String,
    pub key: String,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigErrorResponse {
    pub code: String,
    pub message: String,
}
