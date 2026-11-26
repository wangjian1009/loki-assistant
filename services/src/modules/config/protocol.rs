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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<MonitorMetaResponse>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MonitorMetaResponse {
    pub id: &'static str,
    pub name: String,
    pub scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ItemMetaResponse {
    #[serde(flatten)]
    pub metadata: ConfigItemMetadata,
    #[serde(flatten)]
    pub options: ConfigItemOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_monitor: Option<&'static str>,
}

impl From<ConfigModuleMetadata> for ModuleMetaResponse {
    fn from(module: ConfigModuleMetadata) -> Self {
        let mut monitors = BTreeMap::new();
        let items = module
            .items
            .into_iter()
            .map(|metadata| {
                let options = super::registry::item_options(&module.id, &metadata.key);
                let record = options
                    .structure
                    .as_ref()
                    .is_some_and(|structure| structure.id_field.is_some());
                let monitor = options.monitor.map(|monitor| monitor.id);
                if let Some(id) = monitor {
                    monitors.entry(id).or_insert_with(|| MonitorMetaResponse {
                        id,
                        name: metadata.name.clone(),
                        scope: if record { "record" } else { "module" },
                        item: record.then(|| metadata.key.clone()),
                    });
                }
                ItemMetaResponse {
                    metadata,
                    options,
                    monitor: if record { None } else { monitor },
                    lifecycle_monitor: if record { monitor } else { None },
                }
            })
            .collect();
        Self {
            id: module.id,
            name: module.name,
            description: module.description,
            version: module.version,
            items,
            monitors: monitors.into_values().collect(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
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

#[derive(Deserialize)]
pub struct EditBatchRequest {
    pub changes: Vec<EditChange>,
}

#[derive(Deserialize)]
pub struct EditChange {
    pub path: Vec<String>,
    #[serde(flatten)]
    pub operation: EditOperation,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum EditOperation {
    Set { value: Value },
    Remove,
}
