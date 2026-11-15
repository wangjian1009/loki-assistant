use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use serde_json::{Map, Value};
use tokio::sync::{RwLock, broadcast};

use super::model::{ConfigDataType, ConfigModuleMetadata};

#[derive(Clone, Debug)]
pub struct ConfigService {
    metadata: Arc<RwLock<HashMap<String, ConfigModuleMetadata>>>,
    values: Arc<RwLock<HashMap<String, Map<String, Value>>>>,
    changes: broadcast::Sender<ConfigChange>,
}

#[derive(Clone, Debug)]
pub struct ConfigChange {
    pub module_id: String,
    pub key: String,
    pub value: Value,
    pub operation: ConfigChangeOperation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigChangeOperation {
    Replace,
    Merge,
    Append,
}

pub struct ModuleSubscription {
    module_id: String,
    receiver: broadcast::Receiver<ConfigChange>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ConfigServiceError {
    #[error("未找到配置模块：{0}")]
    ModuleNotFound(String),
    #[error("未找到配置项：{module_id}/{key}")]
    ItemNotFound { module_id: String, key: String },
    #[error("配置项 {module_id}/{key} 尚未设置")]
    ValueNotSet { module_id: String, key: String },
    #[error("配置值无效：{0}")]
    InvalidValue(String),
}

impl ConfigService {
    pub fn new(metadata: ConfigModuleMetadata) -> Self {
        let (changes, _) = broadcast::channel(32);
        let mut modules = HashMap::new();
        modules.insert(metadata.id.clone(), metadata);
        Self {
            metadata: Arc::new(RwLock::new(modules)),
            values: Arc::new(RwLock::new(HashMap::new())),
            changes,
        }
    }

    pub async fn register_module(
        &self,
        metadata: ConfigModuleMetadata,
    ) -> Result<(), ConfigServiceError> {
        let mut modules = self.metadata.write().await;
        if modules.contains_key(&metadata.id) {
            return Err(ConfigServiceError::InvalidValue(format!(
                "配置模块已存在：{}",
                metadata.id
            )));
        }
        modules.insert(metadata.id.clone(), metadata);
        Ok(())
    }

    pub async fn metadata(&self) -> Vec<ConfigModuleMetadata> {
        self.metadata.read().await.values().cloned().collect()
    }

    pub async fn module_values(
        &self,
        module_id: &str,
    ) -> Result<Map<String, Value>, ConfigServiceError> {
        self.module_metadata(module_id).await?;
        Ok(self
            .values
            .read()
            .await
            .get(module_id)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn value(&self, module_id: &str, key: &str) -> Result<Value, ConfigServiceError> {
        self.module_metadata(module_id)
            .await?
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        self.values
            .read()
            .await
            .get(module_id)
            .and_then(|values| values.get(key))
            .cloned()
            .ok_or_else(|| ConfigServiceError::ValueNotSet {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })
    }

    pub async fn replace(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
    ) -> Result<Value, ConfigServiceError> {
        self.validate(module_id, key, &value).await?;
        self.update(module_id, key, value, ConfigChangeOperation::Replace)
            .await
    }

    pub async fn partial(
        &self,
        module_id: &str,
        key: &str,
        partial: Value,
    ) -> Result<Value, ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        let current = self
            .values
            .read()
            .await
            .get(module_id)
            .and_then(|values| values.get(key))
            .cloned();
        let merged = match (&item.data_type, current, partial) {
            (
                ConfigDataType::Dictionary,
                Some(Value::Object(mut current)),
                Value::Object(partial),
            ) => {
                current.extend(partial);
                Value::Object(current)
            }
            (ConfigDataType::Dictionary, None, Value::Object(partial)) => Value::Object(partial),
            (ConfigDataType::List, Some(Value::Array(mut current)), Value::Array(partial)) => {
                current.extend(partial);
                Value::Array(current)
            }
            (ConfigDataType::List, None, Value::Array(partial)) => Value::Array(partial),
            (ConfigDataType::Dictionary, _, _) => {
                return Err(ConfigServiceError::InvalidValue(
                    "字典的部分更新必须是 JSON object".to_owned(),
                ));
            }
            (ConfigDataType::List, _, _) => {
                return Err(ConfigServiceError::InvalidValue(
                    "列表的部分更新必须是 JSON array".to_owned(),
                ));
            }
            _ => {
                return Err(ConfigServiceError::InvalidValue(
                    "只有字典和列表支持部分更新".to_owned(),
                ));
            }
        };
        self.validate(module_id, key, &merged).await?;
        let operation = if item.data_type == ConfigDataType::List {
            ConfigChangeOperation::Append
        } else {
            ConfigChangeOperation::Merge
        };
        self.update(module_id, key, merged, operation).await
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConfigChange> {
        self.changes.subscribe()
    }

    pub fn subscribe_module(&self, module_id: &str) -> ModuleSubscription {
        ModuleSubscription {
            module_id: module_id.to_owned(),
            receiver: self.changes.subscribe(),
        }
    }

    async fn module_metadata(
        &self,
        module_id: &str,
    ) -> Result<ConfigModuleMetadata, ConfigServiceError> {
        self.metadata
            .read()
            .await
            .get(module_id)
            .cloned()
            .ok_or_else(|| ConfigServiceError::ModuleNotFound(module_id.to_owned()))
    }

    async fn validate(
        &self,
        module_id: &str,
        key: &str,
        value: &Value,
    ) -> Result<(), ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        item.validate(value)
    }

    async fn update(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
        operation: ConfigChangeOperation,
    ) -> Result<Value, ConfigServiceError> {
        self.values
            .write()
            .await
            .entry(module_id.to_owned())
            .or_default()
            .insert(key.to_owned(), value.clone());
        let _ = self.changes.send(ConfigChange {
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            value: value.clone(),
            operation,
        });
        Ok(value)
    }
}

impl ModuleSubscription {
    pub async fn recv(&mut self) -> Option<ConfigChange> {
        loop {
            match self.receiver.recv().await {
                Ok(change) if change.module_id == self.module_id => return Some(change),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

impl super::model::ConfigItemMetadata {
    pub fn validate(&self, value: &Value) -> Result<(), ConfigServiceError> {
        if !matches_type(&self.data_type, value, self.list.as_ref()) {
            return Err(ConfigServiceError::InvalidValue(format!(
                "配置项 {} 的值类型不匹配",
                self.key
            )));
        }
        if let Some(checker) = &self.checker {
            let text = value.as_str().ok_or_else(|| {
                ConfigServiceError::InvalidValue(format!(
                    "配置项 {} 的正则检查器需要字符串",
                    self.key
                ))
            })?;
            let regex = Regex::new(&checker.pattern).map_err(|error| {
                ConfigServiceError::InvalidValue(format!("配置项 {} 的正则无效：{error}", self.key))
            })?;
            if !regex.is_match(text) {
                return Err(ConfigServiceError::InvalidValue(format!(
                    "配置项 {} 未通过正则检查",
                    self.key
                )));
            }
        }
        if let Some(dictionary) = &self.dictionary {
            if !dictionary.allow_unknown
                && !dictionary.entries.iter().any(|entry| entry.value == *value)
            {
                return Err(ConfigServiceError::InvalidValue(format!(
                    "配置项 {} 不在允许的字典值中",
                    self.key
                )));
            }
        }
        Ok(())
    }
}

fn matches_type(
    data_type: &ConfigDataType,
    value: &Value,
    list: Option<&super::model::ListMetadata>,
) -> bool {
    match data_type {
        ConfigDataType::String | ConfigDataType::FilePath => value.is_string(),
        ConfigDataType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        ConfigDataType::Number => value.is_number(),
        ConfigDataType::Boolean => value.is_boolean(),
        ConfigDataType::Dictionary => value.is_object(),
        ConfigDataType::List => {
            let Some(items) = value.as_array() else {
                return false;
            };
            let Some(list) = list else { return true };
            list.min_items.is_none_or(|min| items.len() >= min)
                && list.max_items.is_none_or(|max| items.len() <= max)
                && items
                    .iter()
                    .all(|item| matches_type(&list.item_type, item, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::modules::config::model::{ConfigItemMetadata, ConfigModuleMetadata};

    fn metadata() -> ConfigModuleMetadata {
        ConfigModuleMetadata {
            id: "module_a".to_owned(),
            name: "模块 A".to_owned(),
            description: "测试配置模块".to_owned(),
            version: "0.1.0".to_owned(),
            items: vec![
                ConfigItemMetadata {
                    key: "config_a".to_owned(),
                    name: "配置 A".to_owned(),
                    description: "字符串配置".to_owned(),
                    data_type: ConfigDataType::String,
                    required: true,
                    default: Some(json!("默认值")),
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                },
                ConfigItemMetadata {
                    key: "config_b".to_owned(),
                    name: "配置 B".to_owned(),
                    description: "字典配置".to_owned(),
                    data_type: ConfigDataType::Dictionary,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn does_not_return_default_as_configured_value() {
        let service = ConfigService::new(metadata());
        assert_eq!(service.module_values("module_a").await.unwrap().len(), 0);
        assert!(matches!(
            service.value("module_a", "config_a").await,
            Err(ConfigServiceError::ValueNotSet { .. })
        ));

        let mut subscription = service.subscribe_module("module_a");
        service
            .replace("module_a", "config_a", json!("实际值"))
            .await
            .unwrap();
        assert_eq!(subscription.recv().await.unwrap().value, json!("实际值"));
    }

    #[tokio::test]
    async fn partial_update_merges_dictionary() {
        let service = ConfigService::new(metadata());
        service
            .replace("module_a", "config_b", json!({"a": 1}))
            .await
            .unwrap();
        let value = service
            .partial("module_a", "config_b", json!({"b": 2}))
            .await
            .unwrap();
        assert_eq!(value, json!({"a": 1, "b": 2}));
    }
}
