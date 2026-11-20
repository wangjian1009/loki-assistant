use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata, ConfigValue};
use crate::http::error::ApiError;

#[derive(Clone, Debug, Serialize)]
pub struct MetaResponse {
    pub modules: Vec<ModuleMetaResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleMetaResponse {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub items: Vec<ItemMetaResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemMetaResponse {
    #[serde(flatten)]
    pub metadata: ConfigItemMetadata,
    #[serde(flatten)]
    pub options: ConfigItemOptions,
}

impl From<ConfigModuleMetadata> for ModuleMetaResponse {
    fn from(module: ConfigModuleMetadata) -> Self {
        let items = module
            .items
            .into_iter()
            .map(|metadata| {
                let options = super::registry::item_options(&module.id, &metadata.key);
                ItemMetaResponse { metadata, options }
            })
            .collect();
        Self {
            id: module.id,
            name: module.name,
            description: module.description,
            version: module.version,
            items,
        }
    }
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

#[derive(Serialize)]
pub struct RecordResponse {
    pub module_id: String,
    pub key: String,
    pub instance: String,
    pub value: Value,
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
    pub error: Option<ApiError>,
}
