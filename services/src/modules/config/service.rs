use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt::Debug, future::Future, pin::Pin};

use regex::Regex;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast};

use super::{
    model::{
        ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata, ConfigValue, ConfigValueSource,
    },
    storage::{FileStorage, StoredValues},
};

#[derive(Clone, Debug)]
pub struct ConfigService {
    metadata: Arc<RwLock<HashMap<String, ConfigModuleMetadata>>>,
    values: Arc<RwLock<StoredValues>>,
    changes: broadcast::Sender<ConfigChange>,
    commits: Arc<Mutex<()>>,
    handlers: HashMap<String, Arc<dyn ConfigChangeHandler>>,
    storage: Option<FileStorage>,
    executable_search_path: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ConfigChange {
    pub module_id: String,
    pub key: String,
    pub value: Value,
    pub operation: ConfigChangeOperation,
    pub previous_value: Option<Value>,
    /// 本次提交后的模块生效值快照，供关联配置项一起判断；尚未写入内存或文件。
    pub module_values: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigChangeOperation {
    Replace,
    Merge,
    Append,
}

pub type ChangeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// 提交前处理配置变化；不得递归提交配置。失败时处理器负责撤销自身的部分操作。
pub trait ConfigChangeHandler: Debug + Send + Sync {
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_>;

    /// 配置落盘、内存更新后完成外部状态切换；错误只记录，不能撤回已提交配置。
    fn committed(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    /// 处理成功但落盘失败时撤销外部操作；无外部状态的处理器可使用默认实现。
    fn rollback(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
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
    #[error("配置项 {module_id}/{key} 不允许此远端操作")]
    RemoteAccessForbidden { module_id: String, key: String },
    #[error("配置值无效：{0}")]
    InvalidValue(String),
    #[error("配置文件读写失败：{0}")]
    Storage(String),
    #[error("配置变化处理失败：{module_id}/{key}：{message}")]
    ChangeRejected {
        module_id: String,
        key: String,
        message: String,
    },
}

impl ConfigService {
    pub fn new(metadata: ConfigModuleMetadata) -> Self {
        Self::with_metadata(vec![metadata])
    }

    pub fn from_registry() -> Self {
        Self::with_metadata(super::registry::metadata())
    }

    pub fn storage_path() -> Result<PathBuf, ConfigServiceError> {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".assistant/config.yml"))
            .ok_or_else(|| ConfigServiceError::Storage("无法确定用户主目录".to_owned()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigServiceError> {
        Self::load_with_metadata(path, super::registry::metadata())
    }

    pub fn load_with_metadata(
        path: impl AsRef<Path>,
        metadata: Vec<ConfigModuleMetadata>,
    ) -> Result<Self, ConfigServiceError> {
        let storage = FileStorage::new(path)?;
        let values = storage.read()?;
        for (module_id, entries) in &values {
            // 保留其他版本或插件的配置，仅校验当前已知配置项。
            if let Some(module) = metadata.iter().find(|module| &module.id == module_id) {
                for (key, value) in entries {
                    if let Some(item) = module.item(key) {
                        item.validate(value)?;
                    }
                }
            }
        }
        let mut service = Self::with_metadata(metadata);
        service.values = Arc::new(RwLock::new(values));
        service.storage = Some(storage);
        Ok(service)
    }

    /// 在服务组装阶段为模块配置提交处理器，每个模块至多一个。
    pub fn with_change_handler(
        mut self,
        module_id: impl Into<String>,
        handler: Arc<dyn ConfigChangeHandler>,
    ) -> Self {
        self.handlers.insert(module_id.into(), handler);
        self
    }

    pub fn with_executable_search_path(mut self, paths: Vec<PathBuf>) -> Self {
        self.executable_search_path = paths;
        self
    }

    fn with_metadata(metadata: Vec<ConfigModuleMetadata>) -> Self {
        let (changes, _) = broadcast::channel(32);
        let modules = metadata
            .into_iter()
            .map(|metadata| (metadata.id.clone(), metadata))
            .collect();
        Self {
            metadata: Arc::new(RwLock::new(modules)),
            values: Arc::new(RwLock::new(HashMap::new())),
            changes,
            commits: Arc::new(Mutex::new(())),
            handlers: HashMap::new(),
            storage: None,
            executable_search_path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
        }
    }

    pub async fn metadata(&self) -> Vec<ConfigModuleMetadata> {
        self.metadata.read().await.values().cloned().collect()
    }

    pub async fn module_values(
        &self,
        module_id: &str,
    ) -> Result<BTreeMap<String, ConfigValue>, ConfigServiceError> {
        self.module_values_for(module_id, false).await
    }

    pub(super) async fn module_values_for(
        &self,
        module_id: &str,
        remote: bool,
    ) -> Result<BTreeMap<String, ConfigValue>, ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let user_values = self
            .values
            .read()
            .await
            .get(module_id)
            .cloned()
            .unwrap_or_default();
        let mut resolved = BTreeMap::new();
        for item in metadata
            .items
            .into_iter()
            .filter(|item| !remote || item.remote_access.allows_read())
        {
            if let Some(value) = self
                .resolve_value(&item, user_values.get(&item.key))
                .await?
            {
                resolved.insert(item.key, value);
            }
        }
        Ok(resolved)
    }

    pub(super) async fn require_remote_access(
        &self,
        module_id: &str,
        key: &str,
        write: bool,
    ) -> Result<(), ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        if !item.remote_access.allows_read() || (write && !item.remote_access.allows_write()) {
            return Err(ConfigServiceError::RemoteAccessForbidden {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            });
        }
        Ok(())
    }

    pub async fn value(
        &self,
        module_id: &str,
        key: &str,
    ) -> Result<ConfigValue, ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        let user_value = self
            .values
            .read()
            .await
            .get(module_id)
            .and_then(|values| values.get(key))
            .cloned();
        self.resolve_value(item, user_value.as_ref())
            .await?
            .ok_or_else(|| ConfigServiceError::ValueNotSet {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })
    }

    async fn resolve_value(
        &self,
        item: &ConfigItemMetadata,
        user_value: Option<&Value>,
    ) -> Result<Option<ConfigValue>, ConfigServiceError> {
        if let Some(value) = user_value {
            return Ok(Some(ConfigValue {
                value: value.clone(),
                source: ConfigValueSource::User,
            }));
        }
        let default = if let Some(default) = &item.default {
            Some(default.clone())
        } else if item.data_type == ConfigDataType::Executable {
            if let Some(metadata) = &item.executable {
                let metadata = metadata.clone();
                let paths = self.executable_search_path.clone();
                tokio::task::spawn_blocking(move || super::executable::find(&metadata, &paths))
                    .await
                    .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
                    .map(Value::String)
            } else {
                None
            }
        } else {
            None
        };
        Ok(default.map(|value| ConfigValue {
            value,
            source: ConfigValueSource::Default,
        }))
    }

    pub async fn replace(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
    ) -> Result<Value, ConfigServiceError> {
        let value = self.validate_value(module_id, key, &value).await?;
        let service = self.clone();
        let (module_id, key) = (module_id.to_owned(), key.to_owned());
        // HTTP 请求取消后仍完成已接受的落盘事务，避免内存与文件版本分离。
        tokio::spawn(async move {
            service
                .update(&module_id, &key, value, ConfigChangeOperation::Replace)
                .await
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    pub async fn partial(
        &self,
        module_id: &str,
        key: &str,
        partial: Value,
    ) -> Result<Value, ConfigServiceError> {
        let service = self.clone();
        let (module_id, key) = (module_id.to_owned(), key.to_owned());
        tokio::spawn(async move { service.partial_inner(&module_id, &key, partial).await })
            .await
            .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    async fn partial_inner(
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
        let default = self.resolve_value(item, None).await?.map(|data| data.value);
        let _commit = self.commits.lock().await;
        let values = self.values.read().await.clone();
        let current = values
            .get(module_id)
            .and_then(|values| values.get(key))
            .cloned()
            .or(default);
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
        item.validate(&merged)?;
        let operation = if item.data_type == ConfigDataType::List {
            ConfigChangeOperation::Append
        } else {
            ConfigChangeOperation::Merge
        };
        self.commit(values, item, module_id, key, merged, operation)
            .await
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

    pub async fn validate_value(
        &self,
        module_id: &str,
        key: &str,
        value: &Value,
    ) -> Result<Value, ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        item.validate(value)?;
        if item.data_type == ConfigDataType::Executable {
            let input = value.as_str().expect("Executable 类型已经检查").to_owned();
            let paths = self.executable_search_path.clone();
            let suggested = item
                .executable
                .as_ref()
                .map(|metadata| metadata.search_paths.clone())
                .unwrap_or_default();
            let resolved = tokio::task::spawn_blocking(move || {
                super::executable::validate(&input, &paths, &suggested)
            })
            .await
            .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
            .map_err(|message| {
                ConfigServiceError::InvalidValue(format!("{}：{message}", item.name))
            })?;
            return Ok(Value::String(resolved));
        }
        Ok(value.clone())
    }

    async fn update(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
        operation: ConfigChangeOperation,
    ) -> Result<Value, ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        let _commit = self.commits.lock().await;
        let values = self.values.read().await.clone();
        self.commit(values, item, module_id, key, value, operation)
            .await
    }

    async fn commit(
        &self,
        mut values: StoredValues,
        item: &ConfigItemMetadata,
        module_id: &str,
        key: &str,
        value: Value,
        operation: ConfigChangeOperation,
    ) -> Result<Value, ConfigServiceError> {
        let previous_value = self
            .resolve_value(
                item,
                values.get(module_id).and_then(|entries| entries.get(key)),
            )
            .await?
            .map(|data| data.value);
        values
            .entry(module_id.to_owned())
            .or_default()
            .insert(key.to_owned(), value.clone());
        let mut module_values = BTreeMap::new();
        for item in self.module_metadata(module_id).await?.items {
            if let Some(data) = self
                .resolve_value(
                    &item,
                    values
                        .get(module_id)
                        .and_then(|entries| entries.get(&item.key)),
                )
                .await?
            {
                module_values.insert(item.key, data.value);
            }
        }
        let change = ConfigChange {
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            value: value.clone(),
            operation,
            previous_value,
            module_values,
        };
        let handler = self.handlers.get(module_id);
        if let Some(handler) = handler {
            handler.handle(change.clone()).await.map_err(|message| {
                ConfigServiceError::ChangeRejected {
                    module_id: module_id.to_owned(),
                    key: key.to_owned(),
                    message,
                }
            })?;
        }
        if let Some(storage) = &self.storage {
            if let Err(error) = storage.write(values.clone()).await {
                if let Some(handler) = handler {
                    if let Err(message) = handler.rollback(change.clone()).await {
                        return Err(ConfigServiceError::Storage(format!(
                            "{error}；撤销配置处理失败：{message}"
                        )));
                    }
                }
                return Err(error);
            }
        }
        *self.values.write().await = values;
        let _ = self.changes.send(change.clone());
        if let Some(handler) = handler {
            if let Err(error) = handler.committed(change).await {
                eprintln!("配置已保存，完成配置变化处理失败：{module_id}/{key}：{error}");
            }
        }
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
        ConfigDataType::String | ConfigDataType::FilePath | ConfigDataType::Executable => {
            value.is_string()
        }
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
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };

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
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                    required: true,
                    default: Some(json!("默认值")),
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                    executable: None,
                },
                ConfigItemMetadata {
                    key: "config_b".to_owned(),
                    name: "配置 B".to_owned(),
                    description: "字典配置".to_owned(),
                    data_type: ConfigDataType::Dictionary,
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                    executable: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn resolves_defaults_and_distinguishes_explicit_user_values() {
        let service = ConfigService::new(metadata());
        let values = service.module_values("module_a").await.unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(
            values["config_a"],
            ConfigValue {
                value: json!("默认值"),
                source: ConfigValueSource::Default
            }
        );
        assert!(matches!(
            service.value("module_a", "config_b").await,
            Err(ConfigServiceError::ValueNotSet { .. })
        ));

        let mut subscription = service.subscribe_module("module_a");
        service
            .replace("module_a", "config_a", json!("实际值"))
            .await
            .unwrap();
        assert_eq!(subscription.recv().await.unwrap().value, json!("实际值"));
        assert_eq!(
            service.value("module_a", "config_a").await.unwrap(),
            ConfigValue {
                value: json!("实际值"),
                source: ConfigValueSource::User
            }
        );
        service
            .replace("module_a", "config_a", json!("默认值"))
            .await
            .unwrap();
        assert_eq!(
            service.value("module_a", "config_a").await.unwrap().source,
            ConfigValueSource::User
        );
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

    #[derive(Debug, Default)]
    struct TestHandler {
        seen: std::sync::Mutex<Vec<ConfigChange>>,
        reject: std::sync::atomic::AtomicBool,
        service: std::sync::OnceLock<std::sync::Weak<ConfigService>>,
    }

    impl ConfigChangeHandler for TestHandler {
        fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
            Box::pin(async move {
                if let Some(service) = self.service.get().and_then(|service| service.upgrade()) {
                    // 处理器可以读取提交前的值，不被写锁阻塞。
                    let previous = service
                        .value(&change.module_id, &change.key)
                        .await
                        .ok()
                        .map(|data| data.value);
                    assert_eq!(previous, change.previous_value);
                }
                assert_eq!(change.module_values.get(&change.key), Some(&change.value));
                self.seen.lock().unwrap().push(change);
                tokio::task::yield_now().await;
                if self.reject.load(Ordering::SeqCst) {
                    Err("业务拒绝修改".to_owned())
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn handlers_can_read_old_values_and_reject_replace_merge_and_append() {
        for data_type in [ConfigDataType::Dictionary, ConfigDataType::List] {
            let fixture = Fixture::new();
            let path = fixture.0.join("config.yml");
            let mut metadata = metadata();
            metadata.items[1].data_type = data_type.clone();
            metadata.items[1].default = Some(if data_type == ConfigDataType::List {
                json!([0])
            } else {
                json!({"base":true})
            });
            let handler = Arc::new(TestHandler::default());
            let service = Arc::new(
                ConfigService::load_with_metadata(&path, vec![metadata])
                    .unwrap()
                    .with_change_handler("module_a", handler.clone()),
            );
            handler.service.set(Arc::downgrade(&service)).unwrap();
            let mut changes = service.subscribe();
            // 预检不执行处理器。
            service
                .validate_value("module_a", "config_a", &json!("新值"))
                .await
                .unwrap();
            assert!(handler.seen.lock().unwrap().is_empty());
            service
                .replace("module_a", "config_a", json!("旧值"))
                .await
                .unwrap();
            changes.try_recv().unwrap();
            let saved = fs::read(&path).unwrap();
            handler.reject.store(true, Ordering::SeqCst);
            assert!(matches!(
                service.replace("module_a", "config_a", json!("新值")).await,
                Err(ConfigServiceError::ChangeRejected { .. })
            ));
            let partial = if data_type == ConfigDataType::List {
                json!([1])
            } else {
                json!({"extra":1})
            };
            assert!(matches!(
                service
                    .partial("module_a", "config_b", partial.clone())
                    .await,
                Err(ConfigServiceError::ChangeRejected { .. })
            ));
            assert_eq!(fs::read(&path).unwrap(), saved);
            assert_eq!(
                service.value("module_a", "config_a").await.unwrap().value,
                json!("旧值")
            );
            assert_eq!(
                service.value("module_a", "config_b").await.unwrap().source,
                ConfigValueSource::Default
            );
            assert!(changes.try_recv().is_err());
            handler.reject.store(false, Ordering::SeqCst);
            let (first, second) = tokio::join!(
                service.partial("module_a", "config_b", partial.clone()),
                service.partial("module_a", "config_b", partial)
            );
            first.unwrap();
            second.unwrap();
            let expected = if data_type == ConfigDataType::List {
                json!([0, 1, 1])
            } else {
                json!({"base":true,"extra":1})
            };
            assert_eq!(
                service.value("module_a", "config_b").await.unwrap().value,
                expected
            );
            let seen = handler.seen.lock().unwrap();
            assert_eq!(seen.last().unwrap().value, expected);
            assert_eq!(
                seen.last().unwrap().operation,
                if data_type == ConfigDataType::List {
                    ConfigChangeOperation::Append
                } else {
                    ConfigChangeOperation::Merge
                }
            );
        }
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "loki-config-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn executable(&self, directory: &str, executable: bool) -> PathBuf {
            let directory = self.0.join(directory);
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("program");
            fs::write(&path, "测试用文件，不执行").unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn discovers_executable_defaults_in_order_and_user_setting_wins() {
        let fixture = Fixture::new();
        fixture.executable("first", false);
        let expected = fixture.executable("second", true);
        let fallback = fixture.executable("fallback", true);
        let mut metadata = metadata();
        let item = &mut metadata.items[0];
        item.data_type = ConfigDataType::Executable;
        item.default = None;
        item.executable = Some(super::super::model::ExecutableMetadata {
            default_program: "program".to_owned(),
            search_paths: vec![fallback.parent().unwrap().to_str().unwrap().to_owned()],
        });
        let service = ConfigService::new(metadata.clone())
            .with_executable_search_path(vec![fixture.0.join("first"), fixture.0.join("second")]);
        let value = service.value("module_a", "config_a").await.unwrap();
        assert_eq!(
            value,
            ConfigValue {
                value: json!(expected),
                source: ConfigValueSource::Default
            }
        );
        let fallback_service = ConfigService::new(metadata).with_executable_search_path(vec![]);
        assert_eq!(
            fallback_service
                .value("module_a", "config_a")
                .await
                .unwrap()
                .value,
            json!(fallback)
        );
        let custom = fixture.executable("custom", true);
        service
            .replace("module_a", "config_a", json!(custom))
            .await
            .unwrap();
        assert_eq!(
            service.value("module_a", "config_a").await.unwrap(),
            ConfigValue {
                value: json!(custom),
                source: ConfigValueSource::User
            }
        );
        fs::remove_file(fallback).unwrap();
        assert!(matches!(
            fallback_service.value("module_a", "config_a").await,
            Err(ConfigServiceError::ValueNotSet { .. })
        ));
        assert!(
            fallback_service
                .module_values("module_a")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn persists_only_user_values_and_restores_them_after_reload() {
        let fixture = Fixture::new();
        let path = fixture.0.join("nested/config.yml");
        let service = ConfigService::load_with_metadata(&path, vec![metadata()]).unwrap();
        assert_eq!(
            service.value("module_a", "config_a").await.unwrap().source,
            ConfigValueSource::Default
        );
        assert!(!path.exists());
        service
            .replace("module_a", "config_b", json!({"enabled":false,"count":0}))
            .await
            .unwrap();
        let stored: Value = serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            stored,
            json!({"module_a":{"config_b":{"enabled":false,"count":0}}})
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let reloaded = ConfigService::load_with_metadata(&path, vec![metadata()]).unwrap();
        assert_eq!(
            reloaded.value("module_a", "config_b").await.unwrap(),
            ConfigValue {
                value: json!({"enabled":false,"count":0}),
                source: ConfigValueSource::User
            }
        );
        assert_eq!(
            reloaded.value("module_a", "config_a").await.unwrap().source,
            ConfigValueSource::Default
        );
    }

    #[tokio::test]
    async fn failed_write_preserves_memory_and_emits_no_change() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("parent");
        let service =
            ConfigService::load_with_metadata(parent.join("config.yml"), vec![metadata()]).unwrap();
        fs::write(&parent, "阻止创建目录").unwrap();
        let mut subscription = service.subscribe();
        assert!(matches!(
            service.replace("module_a", "config_a", json!("新值")).await,
            Err(ConfigServiceError::Storage(_))
        ));
        assert_eq!(
            service.value("module_a", "config_a").await.unwrap().source,
            ConfigValueSource::Default
        );
        assert!(subscription.try_recv().is_err());
    }

    #[test]
    fn rejects_invalid_yaml_without_replacing_the_file() {
        let fixture = Fixture::new();
        let path = fixture.0.join("config.yml");
        fs::write(&path, "module_a: [broken").unwrap();
        assert!(ConfigService::load_with_metadata(&path, vec![metadata()]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "module_a: [broken");
    }

    #[tokio::test]
    async fn partial_updates_include_defaults_and_serialize_concurrent_writes() {
        let fixture = Fixture::new();
        let mut metadata = metadata();
        metadata.items[1].default = Some(json!({"base":true}));
        let path = fixture.0.join("config.yml");
        let service = ConfigService::load_with_metadata(&path, vec![metadata.clone()]).unwrap();
        let (first, second) = tokio::join!(
            service.partial("module_a", "config_b", json!({"a":1})),
            service.partial("module_a", "config_b", json!({"b":2}))
        );
        first.unwrap();
        second.unwrap();
        let expected = json!({"base":true,"a":1,"b":2});
        assert_eq!(
            service.value("module_a", "config_b").await.unwrap().value,
            expected
        );
        let reloaded = ConfigService::load_with_metadata(&path, vec![metadata]).unwrap();
        assert_eq!(
            reloaded.value("module_a", "config_b").await.unwrap().value,
            expected
        );
    }

    #[tokio::test]
    async fn executable_validation_rejects_invalid_files_without_changing_saved_configuration() {
        let fixture = Fixture::new();
        let valid = fixture.executable("valid", true);
        let not_executable = fixture.executable("not-executable", false);
        let mut metadata = metadata();
        metadata.items[0].data_type = ConfigDataType::Executable;
        metadata.items[0].default = None;
        metadata.items[0].executable = Some(super::super::model::ExecutableMetadata {
            default_program: "program".to_owned(),
            search_paths: vec![valid.parent().unwrap().to_str().unwrap().to_owned()],
        });
        let path = fixture.0.join("config.yml");
        let service = ConfigService::load_with_metadata(&path, vec![metadata])
            .unwrap()
            .with_executable_search_path(vec![]);
        let mut changes = service.subscribe();
        let normalized = service
            .validate_value("module_a", "config_a", &json!("program"))
            .await
            .unwrap();
        assert_eq!(normalized, json!(valid));
        assert!(!path.exists());
        assert!(changes.try_recv().is_err());
        assert_eq!(
            service
                .replace("module_a", "config_a", json!("program"))
                .await
                .unwrap(),
            json!(valid)
        );
        changes.try_recv().unwrap();
        let original = fs::read(&path).unwrap();
        for invalid in [fixture.0.join("missing"), not_executable, fixture.0.clone()] {
            assert!(matches!(
                service
                    .validate_value("module_a", "config_a", &json!(invalid))
                    .await,
                Err(ConfigServiceError::InvalidValue(_))
            ));
            assert!(matches!(
                service
                    .replace("module_a", "config_a", json!(invalid))
                    .await,
                Err(ConfigServiceError::InvalidValue(_))
            ));
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(
                service.value("module_a", "config_a").await.unwrap().value,
                json!(valid)
            );
            assert!(changes.try_recv().is_err());
        }
        let candidate = fixture.executable("candidate", true);
        service
            .validate_value("module_a", "config_a", &json!(candidate))
            .await
            .unwrap();
        fs::remove_file(candidate.clone()).unwrap();
        assert!(
            service
                .replace("module_a", "config_a", json!(candidate))
                .await
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}
