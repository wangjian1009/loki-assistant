use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fmt::Debug, future::Future, pin::Pin};

use regex::Regex;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast};

use crate::modules::password::service::{PasswordError, PasswordService, validate_file};

use super::{
    model::{
        ActionInteraction, ConfigDataType, ConfigItemMetadata, ConfigItemOptions,
        ConfigModuleMetadata, ConfigValue, ConfigValueSource,
    },
    registry::item_options,
    storage::{FileStorage, StoredValues},
};

mod notifications;
pub(crate) use notifications::NotificationRequest;

#[derive(Clone, Debug)]
pub struct ConfigService {
    metadata: Arc<RwLock<HashMap<String, ConfigModuleMetadata>>>,
    values: Arc<RwLock<StoredValues>>,
    changes: broadcast::Sender<ConfigChange>,
    commits: Arc<Mutex<()>>,
    handlers: HashMap<String, Arc<dyn ConfigChangeHandler>>,
    passwords: Option<Arc<PasswordService>>,
    storage: Option<FileStorage>,
    executable_search_path: Vec<PathBuf>,
    notification_router: Arc<std::sync::OnceLock<std::sync::Weak<axum::Router>>>,
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
    Remove,
    AuthorizedCreate,
}

pub type ChangeFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
pub type ProcessFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;
pub type ActionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, ConfigServiceError>> + Send + 'a>>;

/// 提交前处理配置变化；不得递归提交配置。失败时处理器负责撤销自身的部分操作。
pub trait ConfigChangeHandler: Debug + Send + Sync {
    fn handle(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    /// 返回应保存的完整配置值。现有校验处理器可继续实现 handle。
    fn process<'a>(&'a self, change: ConfigChange) -> ProcessFuture<'a> {
        Box::pin(async move {
            let value = change.value.clone();
            self.handle(change).await?;
            Ok(value)
        })
    }

    fn append_records<'a>(&'a self, change: ConfigChange) -> ProcessFuture<'a> {
        self.process(change)
    }

    fn remove_records<'a>(&'a self, change: ConfigChange) -> ProcessFuture<'a> {
        self.process(change)
    }

    fn secret_is_set<'a>(
        &'a self,
        _key: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }

    fn authorize_secret<'a>(&'a self, _bearer: Option<&'a str>) -> ChangeFuture<'a> {
        Box::pin(async { Err("此配置项需要本机凭据。".into()) })
    }

    fn write_secret<'a>(&'a self, _key: &'a str, _value: Value) -> ChangeFuture<'a> {
        Box::pin(async { Err("此配置项没有注册凭据存储。".into()) })
    }

    fn validate_secret<'a>(&'a self, _key: &'a str, _value: &'a Value) -> ChangeFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn action<'a>(
        &'a self,
        _action: &'a str,
        _instance: &'a str,
        _config: std::sync::Weak<ConfigService>,
    ) -> ActionFuture<'a> {
        Box::pin(async { Err(ConfigServiceError::ActionNotFound) })
    }

    /// 注册需要由模块处理新增的配置项。
    fn record_creation_keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn prepare_record<'a>(
        &'a self,
        _key: &'a str,
        _instance: &'a str,
        _value: Value,
        _config: std::sync::Weak<ConfigService>,
    ) -> ActionFuture<'a> {
        Box::pin(async { Err(ConfigServiceError::ActionNotFound) })
    }

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
    #[error("此配置项需要有效的本机凭据。")]
    SecretUnauthorized,
    #[error("未找到配置操作或操作记录。")]
    ActionNotFound,
    #[error("该配置记录已存在。")]
    RecordExists,
    #[error("请先完成配置记录的授权。")]
    RecordAuthorizationRequired,
    #[error("该配置已有待确认的变更。")]
    ChangePending,
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
        let mut values = storage.read()?;
        for (module_id, entries) in &mut values {
            // 保留其他版本或插件的配置，仅校验当前已知配置项。
            if let Some(module) = metadata.iter().find(|module| &module.id == module_id) {
                entries.retain(|key, _| {
                    module
                        .item(key)
                        .is_none_or(|item| item.data_type != ConfigDataType::Password)
                });
                for item in &module.items {
                    let options = item_options(module_id, &item.key);
                    if let Some(legacy) = options
                        .structure
                        .as_ref()
                        .and_then(|structure| structure.legacy_dictionary.as_ref())
                        && !entries.contains_key(&item.key)
                        && let Some(previous) = entries.remove(&legacy.key)
                    {
                        entries.insert(item.key.clone(), options.migrate_dictionary(previous)?);
                    }
                }
                for (key, value) in entries.iter() {
                    if let Some(item) = module.item(key) {
                        item.validate_with(&item_options(module_id, key), value)?;
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

    pub(crate) fn with_password_service(mut self, passwords: Arc<PasswordService>) -> Self {
        self.passwords = Some(passwords);
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
            passwords: None,
            storage: None,
            notification_router: Arc::default(),
            executable_search_path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
        }
    }

    pub async fn metadata(&self) -> Vec<ConfigModuleMetadata> {
        self.metadata.read().await.values().cloned().collect()
    }

    pub async fn action(
        self: &Arc<Self>,
        module: &str,
        action: &str,
        instance: &str,
        bearer: Option<&str>,
    ) -> Result<Value, ConfigServiceError> {
        let handler = self
            .handlers
            .get(module)
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let metadata = self.module_metadata(module).await?;
        let action = metadata
            .items
            .iter()
            .flat_map(|item| item_options(module, &item.key).actions)
            .find(|item| item.id == action)
            .ok_or(ConfigServiceError::ActionNotFound)?;
        if !matches!(action.interaction, ActionInteraction::Request) {
            return Err(ConfigServiceError::ActionNotFound);
        }
        handler
            .authorize_secret(bearer)
            .await
            .map_err(|_| ConfigServiceError::SecretUnauthorized)?;
        handler
            .action(&action.id, instance, Arc::downgrade(self))
            .await
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
            let value = if item.data_type == ConfigDataType::Password {
                Some(self.secret_value(module_id, &item.key).await)
            } else {
                self.resolve_value(&item, user_values.get(&item.key))
                    .await?
            };
            if let Some(value) = value {
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
        if item.data_type == ConfigDataType::Password {
            return Ok(self.secret_value(module_id, key).await);
        }
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
        let metadata = self.module_metadata(module_id).await?;
        let item = metadata
            .item(key)
            .ok_or_else(|| ConfigServiceError::ItemNotFound {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
            })?;
        let value = self.normalize_value(module_id, item, &value).await?;
        if self.notifications_attached()
            && item_options(module_id, key).notification_endpoint.is_some()
        {
            return Err(ConfigServiceError::InvalidValue(
                "请使用带通知的配置写入接口。".into(),
            ));
        }
        let secret = item.data_type == ConfigDataType::Password;
        let service = self.clone();
        let (module_id, key) = (module_id.to_owned(), key.to_owned());
        // HTTP 请求取消后仍完成已接受的落盘事务，避免内存与文件版本分离。
        tokio::spawn(async move {
            if secret {
                service.replace_secret(&module_id, &key, value).await
            } else {
                service
                    .update(&module_id, &key, value, ConfigChangeOperation::Replace)
                    .await
            }
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

    pub(super) fn has_record_creation_handler(&self, module: &str, key: &str) -> bool {
        self.handlers
            .get(module)
            .is_some_and(|handler| handler.record_creation_keys().contains(&key))
    }

    pub(super) async fn prepare_record(
        self: &Arc<Self>,
        module: &str,
        key: &str,
        instance: &str,
        value: Value,
    ) -> Result<Value, ConfigServiceError> {
        let handler = self
            .handlers
            .get(module)
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let metadata = self.module_metadata(module).await?;
        let item = metadata
            .item(key)
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let options = item_options(module, key);
        let id_field = options
            .structure
            .as_ref()
            .and_then(|structure| structure.id_field.as_ref())
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let mut record = value.as_object().cloned().ok_or_else(|| {
            ConfigServiceError::InvalidValue("配置记录必须为 JSON object。".into())
        })?;
        if record
            .get(id_field)
            .is_some_and(|id| id.as_str() != Some(instance))
        {
            return Err(ConfigServiceError::InvalidValue(
                "配置记录标识不一致。".into(),
            ));
        }
        if options.structure.as_ref().is_some_and(|structure| {
            structure
                .fields
                .iter()
                .any(|field| field.read_only && record.contains_key(&field.key))
        }) {
            return Err(ConfigServiceError::InvalidValue(
                "配置记录包含只读字段。".into(),
            ));
        }
        record.insert(id_field.clone(), Value::String(instance.into()));
        let record = Value::Object(record);
        let current = self.value(module, key).await?.value;
        let mut records = current
            .as_array()
            .cloned()
            .ok_or(ConfigServiceError::ActionNotFound)?;
        if records
            .iter()
            .any(|value| value[id_field].as_str() == Some(instance))
        {
            return Err(ConfigServiceError::RecordExists);
        }
        records.push(record.clone());
        let candidate = Value::Array(records);
        item.validate_with(&options, &candidate)?;
        let mut module_values: BTreeMap<_, _> = self
            .module_values(module)
            .await?
            .into_iter()
            .map(|(key, value)| (key, value.value))
            .collect();
        module_values.insert(key.into(), candidate.clone());
        let change = ConfigChange {
            module_id: module.into(),
            key: key.into(),
            value: candidate,
            operation: ConfigChangeOperation::AuthorizedCreate,
            previous_value: Some(current),
            module_values,
        };
        handler
            .handle(change)
            .await
            .map_err(|message| ConfigServiceError::ChangeRejected {
                module_id: module.into(),
                key: key.into(),
                message,
            })?;
        handler
            .prepare_record(key, instance, record, Arc::downgrade(self))
            .await
    }

    pub async fn replace_record(
        &self,
        module_id: String,
        key: String,
        instance: String,
        value: Value,
        create_only: bool,
        authorized: bool,
    ) -> Result<Value, ConfigServiceError> {
        self.replace_record_inner(
            module_id,
            key,
            instance,
            Some(value),
            create_only,
            authorized,
            None,
        )
        .await
    }

    /// 将已经由所属模块验证的凭据写入此字段注册的密码条目。
    pub(crate) async fn save_record_password(
        &self,
        module: &str,
        key: &str,
        instance: &str,
        field: &str,
        new_record: Option<Value>,
        secret: String,
    ) -> Result<Value, ConfigServiceError> {
        let create_only = new_record.is_some();
        self.replace_record_inner(
            module.to_owned(),
            key.to_owned(),
            instance.to_owned(),
            new_record,
            create_only,
            true,
            Some((field.to_owned(), secret)),
        )
        .await
    }

    /// 从已保存记录对应的密码条目读取凭据，供模块内部使用。
    pub(crate) async fn read_record_password(
        &self,
        module: &str,
        key: &str,
        instance: &str,
        field: &str,
    ) -> Result<String, ConfigServiceError> {
        let options = item_options(module, key);
        let id_field = options
            .structure
            .as_ref()
            .and_then(|value| value.id_field.as_ref())
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let data = self.value(module, key).await?;
        let path = data
            .value
            .as_array()
            .and_then(|records| {
                records
                    .iter()
                    .find(|record| record[id_field].as_str() == Some(instance))
            })
            .and_then(|record| record[field].as_str())
            .filter(|path| options.matches_password_path(field, instance, path))
            .ok_or(ConfigServiceError::ActionNotFound)?;
        self.passwords
            .as_ref()
            .ok_or_else(|| ConfigServiceError::Storage("密码模块不可用。".into()))?
            .fetch(path)
            .await
            .map_err(|error| ConfigServiceError::Storage(error.to_string()))
    }

    async fn replace_record_inner(
        &self,
        module_id: String,
        key: String,
        instance: String,
        value: Option<Value>,
        create_only: bool,
        authorized: bool,
        password: Option<(String, String)>,
    ) -> Result<Value, ConfigServiceError> {
        let preserve_record = value.is_none();
        let value = value.unwrap_or_else(|| serde_json::json!({}));
        let metadata = self.module_metadata(&module_id).await?;
        let not_found = || ConfigServiceError::ItemNotFound {
            module_id: module_id.clone(),
            key: key.clone(),
        };
        let item = metadata
            .item(&key)
            .filter(|item| item.data_type == ConfigDataType::List)
            .ok_or_else(not_found)?;
        let options = item_options(&module_id, &key);
        let id_field = options
            .structure
            .as_ref()
            .and_then(|structure| structure.id_field.clone())
            .ok_or_else(not_found)?;
        let mut record = value.as_object().cloned().ok_or_else(|| {
            ConfigServiceError::InvalidValue("配置记录必须为 JSON object。".into())
        })?;
        let read_only: Vec<_> = options
            .structure
            .iter()
            .flat_map(|structure| &structure.fields)
            .filter(|field| field.read_only)
            .map(|field| field.key.clone())
            .collect();
        if !authorized && read_only.iter().any(|key| record.contains_key(key)) {
            return Err(ConfigServiceError::InvalidValue(
                "配置记录包含只读字段。".into(),
            ));
        }
        if instance.trim().is_empty()
            || instance.chars().any(char::is_control)
            || record
                .get(&id_field)
                .is_some_and(|id| id.as_str() != Some(instance.as_str()))
        {
            return Err(ConfigServiceError::InvalidValue(
                "配置记录标识无效。".into(),
            ));
        }
        record.insert(id_field.clone(), Value::String(instance.clone()));
        let password = password
            .map(|(field, secret)| {
                let path = options.password_path(&field, &instance)?;
                record.insert(field.clone(), Value::String(path.clone()));
                Ok::<_, ConfigServiceError>((field, path, secret))
            })
            .transpose()?;
        let default = self.resolve_value(item, None).await?.map(|data| data.value);
        let item = item.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let _commit = service.commits.lock().await;
            let values = service.values.read().await.clone();
            let mut collection = match values
                .get(&module_id)
                .and_then(|entries| entries.get(&key))
                .cloned()
                .or(default)
            {
                Some(Value::Array(records)) => records,
                None => Vec::new(),
                _ => {
                    return Err(ConfigServiceError::InvalidValue(
                        "配置集合必须为 JSON array。".into(),
                    ));
                }
            };
            let created = if let Some(current) = collection
                .iter_mut()
                .find(|value| value[&id_field].as_str() == Some(instance.as_str()))
            {
                if create_only {
                    return Err(ConfigServiceError::RecordExists);
                }
                if preserve_record {
                    let mut previous = current.as_object().cloned().ok_or_else(|| {
                        ConfigServiceError::InvalidValue("配置记录必须为 JSON object。".into())
                    })?;
                    previous.extend(record);
                    record = previous;
                }
                if !authorized {
                    for key in &read_only {
                        if let Some(value) = current.get(key) {
                            record.insert(key.clone(), value.clone());
                        }
                    }
                }
                if let Some(previous_id) = options
                    .structure
                    .as_ref()
                    .and_then(|structure| structure.legacy_dictionary.as_ref())
                    .and_then(|legacy| legacy.previous_id_field.as_deref())
                    .and_then(|key| current.get(key).cloned().map(|value| (key, value)))
                {
                    record.insert(previous_id.0.to_owned(), previous_id.1);
                }
                let record = Value::Object(record);
                *current = record;
                false
            } else {
                if preserve_record {
                    return Err(ConfigServiceError::ItemNotFound { module_id, key });
                }
                collection.push(Value::Object(record));
                true
            };
            let collection = Value::Array(collection);
            item.validate_with(&options, &collection)?;
            let stored = service
                .commit_with_password(
                    values,
                    &item,
                    &module_id,
                    &key,
                    collection,
                    if authorized {
                        ConfigChangeOperation::AuthorizedCreate
                    } else if created {
                        ConfigChangeOperation::Append
                    } else {
                        ConfigChangeOperation::Merge
                    },
                    password,
                )
                .await?;
            stored
                .as_array()
                .and_then(|records| {
                    records
                        .iter()
                        .find(|record| record[&id_field].as_str() == Some(instance.as_str()))
                })
                .cloned()
                .ok_or_else(|| ConfigServiceError::Storage("保存结果缺少目标配置记录。".into()))
        })
        .await
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?
    }

    pub(super) async fn remove_record(
        &self,
        module_id: String,
        key: String,
        instance: String,
    ) -> Result<(), ConfigServiceError> {
        let metadata = self.module_metadata(&module_id).await?;
        let not_found = || ConfigServiceError::ItemNotFound {
            module_id: module_id.clone(),
            key: key.clone(),
        };
        let item = metadata
            .item(&key)
            .filter(|item| item.data_type == ConfigDataType::List)
            .ok_or_else(not_found)?;
        let options = item_options(&module_id, &key);
        let id_field = options
            .structure
            .as_ref()
            .and_then(|structure| structure.id_field.clone())
            .ok_or_else(not_found)?;
        let default = self.resolve_value(item, None).await?.map(|data| data.value);
        let item = item.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let _commit = service.commits.lock().await;
            let values = service.values.read().await.clone();
            let Some(Value::Array(mut records)) = values
                .get(&module_id)
                .and_then(|entries| entries.get(&key))
                .cloned()
                .or(default)
            else {
                return Ok(());
            };
            let Some(index) = records
                .iter()
                .position(|record| record[&id_field].as_str() == Some(instance.as_str()))
            else {
                return Ok(());
            };
            records.remove(index);
            let value = Value::Array(records);
            item.validate_with(&options, &value)?;
            service
                .commit(
                    values,
                    &item,
                    &module_id,
                    &key,
                    value,
                    ConfigChangeOperation::Remove,
                )
                .await?;
            Ok(())
        })
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
        item.validate_with(&item_options(module_id, key), &merged)?;
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
        let value = self.normalize_value(module_id, item, value).await?;
        if item.data_type == ConfigDataType::Password {
            if let Some(handler) = self.handlers.get(module_id) {
                handler
                    .validate_secret(key, &value)
                    .await
                    .map_err(ConfigServiceError::InvalidValue)?;
            }
            Ok(Value::Bool(true))
        } else {
            Ok(value)
        }
    }

    pub async fn authorize_secret_request(
        &self,
        module_id: &str,
        key: &str,
        bearer: Option<&str>,
    ) -> Result<(), ConfigServiceError> {
        let metadata = self.module_metadata(module_id).await?;
        if metadata.item(key).is_some_and(|item| {
            item.data_type == ConfigDataType::Password
                || item_options(module_id, key).requires_local_auth
        }) {
            let handler = self
                .handlers
                .get(module_id)
                .ok_or(ConfigServiceError::SecretUnauthorized)?;
            handler
                .authorize_secret(bearer)
                .await
                .map_err(|_| ConfigServiceError::SecretUnauthorized)?;
        }
        Ok(())
    }

    async fn secret_value(&self, module_id: &str, key: &str) -> ConfigValue {
        let is_set = match self.handlers.get(module_id) {
            Some(handler) => handler.secret_is_set(key).await,
            None => false,
        };
        ConfigValue {
            value: Value::Bool(is_set),
            source: if is_set {
                ConfigValueSource::User
            } else {
                ConfigValueSource::Default
            },
        }
    }

    async fn replace_secret(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
    ) -> Result<Value, ConfigServiceError> {
        let _commit = self.commits.lock().await;
        let rejected = |message| ConfigServiceError::ChangeRejected {
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            message,
        };
        let handler = self
            .handlers
            .get(module_id)
            .ok_or_else(|| rejected("此配置项没有注册凭据存储。".into()))?;
        let mut module_values: BTreeMap<_, _> = self
            .module_values(module_id)
            .await?
            .into_iter()
            .map(|(key, value)| (key, value.value))
            .collect();
        let previous_value = module_values.insert(key.to_owned(), Value::Bool(true));
        handler.write_secret(key, value).await.map_err(rejected)?;
        let _ = self.changes.send(ConfigChange {
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            value: Value::Bool(true),
            operation: ConfigChangeOperation::Replace,
            previous_value,
            module_values,
        });
        Ok(Value::Bool(true))
    }

    async fn normalize_value(
        &self,
        module_id: &str,
        item: &ConfigItemMetadata,
        value: &Value,
    ) -> Result<Value, ConfigServiceError> {
        item.validate_with(&item_options(module_id, &item.key), value)?;
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
        values: StoredValues,
        item: &ConfigItemMetadata,
        module_id: &str,
        key: &str,
        value: Value,
        operation: ConfigChangeOperation,
    ) -> Result<Value, ConfigServiceError> {
        self.commit_with_password(values, item, module_id, key, value, operation, None)
            .await
    }

    async fn commit_with_password(
        &self,
        mut values: StoredValues,
        item: &ConfigItemMetadata,
        module_id: &str,
        key: &str,
        value: Value,
        operation: ConfigChangeOperation,
        password: Option<(String, String, String)>,
    ) -> Result<Value, ConfigServiceError> {
        let previous_value = self
            .resolve_value(
                item,
                values.get(module_id).and_then(|entries| entries.get(key)),
            )
            .await?
            .map(|data| data.value);
        let options = item_options(module_id, key);
        if options.notification_endpoint.is_some()
            && self.notifications_attached()
            && value.is_array()
            && operation != ConfigChangeOperation::AuthorizedCreate
        {
            return Err(ConfigServiceError::InvalidValue(
                "请逐条提交带通知的配置记录。".into(),
            ));
        }
        if operation != ConfigChangeOperation::AuthorizedCreate
            && self.has_record_creation_handler(module_id, key)
            && let Some(id_field) = options
                .structure
                .as_ref()
                .and_then(|structure| structure.id_field.as_ref())
            && let Some(records) = value.as_array()
            && records.iter().any(|record| {
                !previous_value
                    .as_ref()
                    .and_then(Value::as_array)
                    .is_some_and(|existing| {
                        existing.iter().any(|old| old[id_field] == record[id_field])
                    })
            })
        {
            return Err(ConfigServiceError::RecordAuthorizationRequired);
        }
        if operation != ConfigChangeOperation::AuthorizedCreate
            && let Some(structure) = &options.structure
            && let Some(id_field) = &structure.id_field
            && let Some(records) = value.as_array()
        {
            let previous = previous_value.as_ref().and_then(Value::as_array);
            for record in records {
                let old = previous.and_then(|records| {
                    records.iter().find(|old| old[id_field] == record[id_field])
                });
                if structure
                    .fields
                    .iter()
                    .filter(|field| field.read_only)
                    .any(|field| {
                        record.get(&field.key) != old.and_then(|record| record.get(&field.key))
                    })
                {
                    return Err(ConfigServiceError::InvalidValue(
                        "不能修改配置记录的只读字段。".into(),
                    ));
                }
            }
        }
        values
            .entry(module_id.to_owned())
            .or_default()
            .insert(key.to_owned(), value.clone());
        let mut module_values = BTreeMap::new();
        for item in self.module_metadata(module_id).await?.items {
            if item.data_type == ConfigDataType::Password {
                module_values.insert(
                    item.key.clone(),
                    self.secret_value(module_id, &item.key).await.value,
                );
                continue;
            }
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
        let operation = match (&operation, previous_value.as_ref(), value.as_array()) {
            (ConfigChangeOperation::Replace, previous, Some(candidate))
                if item.data_type == ConfigDataType::List =>
            {
                let previous = previous
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                list_operation(
                    previous,
                    candidate,
                    options
                        .structure
                        .as_ref()
                        .and_then(|structure| structure.id_field.as_deref()),
                )
            }
            _ => operation,
        };
        let mut change = ConfigChange {
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            operation,
            previous_value,
            module_values,
            value,
        };
        let handler = self.handlers.get(module_id);
        if let Some(handler) = handler {
            let rejected = |message| ConfigServiceError::ChangeRejected {
                module_id: module_id.to_owned(),
                key: key.to_owned(),
                message,
            };
            let processed = match change.operation {
                ConfigChangeOperation::Append | ConfigChangeOperation::AuthorizedCreate => {
                    handler.append_records(change.clone()).await
                }
                ConfigChangeOperation::Remove => handler.remove_records(change.clone()).await,
                _ => handler.process(change.clone()).await,
            }
            .map_err(rejected)?;
            let normalized = self
                .normalize_value(module_id, item, &processed)
                .await
                .and_then(|processed| {
                    if let Some(id_field) = options
                        .structure
                        .as_ref()
                        .and_then(|structure| structure.id_field.as_ref())
                        && let (Some(candidate), Some(result)) =
                            (change.value.as_array(), processed.as_array())
                        && (candidate.len() != result.len()
                            || candidate.iter().any(|record| {
                                !result
                                    .iter()
                                    .any(|saved| saved[id_field] == record[id_field])
                            }))
                    {
                        return Err(ConfigServiceError::InvalidValue(
                            "处理器修改了配置记录主键。".into(),
                        ));
                    }
                    if let Some((field, path, _)) = &password
                        && !processed.as_array().is_some_and(|records| {
                            records
                                .iter()
                                .any(|record| record[field].as_str() == Some(path.as_str()))
                        })
                    {
                        return Err(ConfigServiceError::InvalidValue(
                            "处理器移除了密码字段引用。".into(),
                        ));
                    }
                    Ok(processed)
                });
            let processed = match normalized {
                Ok(processed) => processed,
                Err(error) => {
                    let mut message = error.to_string();
                    if let Err(rollback) = handler.rollback(change).await {
                        message.push_str(&format!("，撤销配置处理失败：{rollback}"));
                    }
                    return Err(rejected(message));
                }
            };
            values
                .entry(module_id.to_owned())
                .or_default()
                .insert(key.to_owned(), processed.clone());
            change
                .module_values
                .insert(key.to_owned(), processed.clone());
            change.value = processed;
        }
        let saved = change.value.clone();
        let previous_password = if let Some((_, path, secret)) = password {
            let result = async {
                let store = self
                    .passwords
                    .as_ref()
                    .ok_or_else(|| ConfigServiceError::Storage("密码模块不可用。".into()))?;
                let previous = match store.fetch(&path).await {
                    Ok(previous) => Some(previous),
                    Err(PasswordError::EntryNotFound) => None,
                    Err(error) => return Err(ConfigServiceError::Storage(error.to_string())),
                };
                store
                    .save(&path, &secret)
                    .await
                    .map_err(|error| ConfigServiceError::Storage(error.to_string()))?;
                Ok((store, path, previous))
            }
            .await;
            match result {
                Ok(previous) => Some(previous),
                Err(error) => {
                    if let Some(handler) = handler {
                        let _ = handler.rollback(change).await;
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        if let Some(storage) = &self.storage {
            if let Err(error) = storage.write(values.clone()).await {
                let password_rollback = if let Some((store, path, previous)) = previous_password {
                    if let Some(previous) = previous {
                        store.save(&path, previous.trim_end_matches('\n')).await
                    } else {
                        store.remove(&path).await
                    }
                } else {
                    Ok(())
                };
                if let Some(handler) = handler {
                    if let Err(message) = handler.rollback(change.clone()).await {
                        return Err(ConfigServiceError::Storage(format!(
                            "{error}；撤销配置处理失败：{message}"
                        )));
                    }
                }
                if let Err(rollback) = password_rollback {
                    return Err(ConfigServiceError::Storage(format!(
                        "{error}。撤销密码写入失败：{rollback}"
                    )));
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
        Ok(saved)
    }
}

// 全量替换恰好只新增或删除成员时，也交给对应的数组处理器。
fn list_operation(
    previous: &[Value],
    candidate: &[Value],
    id_field: Option<&str>,
) -> ConfigChangeOperation {
    use std::cmp::Ordering;
    let (larger, smaller, operation) = match previous.len().cmp(&candidate.len()) {
        Ordering::Less => (candidate, previous, ConfigChangeOperation::Append),
        Ordering::Greater => (previous, candidate, ConfigChangeOperation::Remove),
        Ordering::Equal => return ConfigChangeOperation::Replace,
    };
    let mut unmatched: Vec<_> = larger.iter().collect();
    for item in smaller {
        let Some(index) = unmatched
            .iter()
            .position(|value| id_field.map_or(*value == item, |field| value[field] == item[field]))
        else {
            return ConfigChangeOperation::Replace;
        };
        unmatched.swap_remove(index);
    }
    operation
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

impl ConfigItemOptions {
    fn notification_key_field(&self) -> Result<&str, ConfigServiceError> {
        let structure = self
            .structure
            .as_ref()
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let mut keys = structure
            .fields
            .iter()
            .filter(|field| field.data_type == ConfigDataType::Key);
        match (keys.next(), keys.next()) {
            (Some(field), None)
                if field.required && structure.id_field.as_deref() == Some(&field.key) =>
            {
                Ok(&field.key)
            }
            _ => Err(ConfigServiceError::InvalidValue(
                "通知记录需要一个必填的 Key 主键字段。".into(),
            )),
        }
    }

    fn matches_password_path(&self, field: &str, instance: &str, path: &str) -> bool {
        let Ok(base) = self.password_path(field, instance) else {
            return false;
        };
        if path == base {
            return true;
        }
        self.notification_endpoint.is_some()
            && path
                .strip_prefix(base.trim_end_matches(".gpg"))
                .and_then(|suffix| suffix.strip_prefix('-'))
                .and_then(|suffix| suffix.strip_suffix(".gpg"))
                .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
    }

    fn password_path(&self, key: &str, instance: &str) -> Result<String, ConfigServiceError> {
        let template =
            self.structure
                .as_ref()
                .and_then(|structure| {
                    structure.fields.iter().find(|field| {
                        field.key == key && field.data_type == ConfigDataType::Password
                    })
                })
                .and_then(|field| field.password_path.as_deref())
                .ok_or(ConfigServiceError::ActionNotFound)?;
        if !template.contains("{id}")
            || instance.is_empty()
            || !instance
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ConfigServiceError::InvalidValue(
                "密码字段存储路径无效。".into(),
            ));
        }
        let path = template.replace("{id}", instance);
        validate_file(&path)
            .map_err(|error| ConfigServiceError::InvalidValue(error.to_string()))?;
        Ok(path)
    }

    fn migrate_dictionary(&self, value: Value) -> Result<Value, ConfigServiceError> {
        let structure = self.structure.as_ref().expect("迁移结构已声明");
        let legacy = structure
            .legacy_dictionary
            .as_ref()
            .expect("字典迁移已声明");
        let entries = value.as_object().ok_or_else(|| {
            ConfigServiceError::InvalidValue("旧版配置集合必须为 JSON object。".into())
        })?;
        let mut records = Vec::with_capacity(entries.len());
        for (id, value) in entries {
            let mut record = value.as_object().cloned().ok_or_else(|| {
                ConfigServiceError::InvalidValue("旧版配置记录必须为 JSON object。".into())
            })?;
            record.retain(|key, _| structure.fields.iter().any(|field| &field.key == key));
            if let Some(id_field) = &structure.id_field {
                record
                    .entry(id_field.clone())
                    .or_insert_with(|| Value::String(id.clone()));
            }
            if let Some(previous_id_field) = &legacy.previous_id_field {
                record.insert(previous_id_field.clone(), Value::String(id.clone()));
            }
            records.push(Value::Object(record));
        }
        Ok(Value::Array(records))
    }
}

impl ConfigItemMetadata {
    pub fn validate(&self, value: &Value) -> Result<(), ConfigServiceError> {
        self.validate_with(&ConfigItemOptions::default(), value)
    }

    fn validate_with(
        &self,
        options: &ConfigItemOptions,
        value: &Value,
    ) -> Result<(), ConfigServiceError> {
        if !matches_type(&self.data_type, value, self.list.as_ref()) {
            return Err(ConfigServiceError::InvalidValue(format!(
                "配置项 {} 的值类型不匹配",
                self.key
            )));
        }
        if let Some(structure) = &options.structure {
            let records: &[Value] = match self.data_type {
                ConfigDataType::Structure => std::slice::from_ref(value),
                ConfigDataType::List => value.as_array().expect("列表已经通过类型检查"),
                _ => &[],
            };
            let mut ids = std::collections::BTreeSet::new();
            for record in records {
                for field in &structure.fields {
                    match record.get(&field.key) {
                        Some(value)
                            if matches_type(&field.data_type, value, None)
                                && (!field.required
                                    || !value
                                        .as_str()
                                        .is_some_and(|text| text.trim().is_empty())) =>
                        {
                            if let Some(checker) = &field.checker {
                                checker.validate(&field.label, value)?;
                            }
                        }
                        None if !field.required => {}
                        _ => {
                            return Err(ConfigServiceError::InvalidValue(format!(
                                "请检查字段 {}。",
                                field.label
                            )));
                        }
                    }
                }
                for field in structure
                    .fields
                    .iter()
                    .filter(|field| field.password_path.is_some())
                {
                    if let Some(value) = record.get(&field.key) {
                        let instance = structure
                            .id_field
                            .as_ref()
                            .and_then(|key| record[key].as_str())
                            .ok_or_else(|| {
                                ConfigServiceError::InvalidValue("密码字段需要记录标识。".into())
                            })?;
                        if !value.as_str().is_some_and(|path| {
                            options.matches_password_path(&field.key, instance, path)
                        }) {
                            return Err(ConfigServiceError::InvalidValue(
                                "密码字段需要有效的存储引用。".into(),
                            ));
                        }
                    }
                }
                if let Some(field) = &structure.id_field {
                    let id = record
                        .get(field)
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control));
                    if !id.is_some_and(|id| ids.insert(id)) {
                        return Err(ConfigServiceError::InvalidValue(
                            "配置记录标识缺失或重复。".into(),
                        ));
                    }
                }
            }
        }
        if let Some(checker) = &self.checker {
            checker.validate(&self.key, value)?;
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

impl super::model::CheckerMetadata {
    fn validate(&self, label: &str, value: &Value) -> Result<(), ConfigServiceError> {
        let expression = Regex::new(&self.pattern).map_err(|_| {
            ConfigServiceError::InvalidValue(format!("配置项 {label} 的正则无效。"))
        })?;
        if value.as_str().is_some_and(|text| expression.is_match(text)) {
            Ok(())
        } else {
            Err(ConfigServiceError::InvalidValue(format!(
                "{label}：{}",
                self.description
            )))
        }
    }
}

fn matches_type(
    data_type: &ConfigDataType,
    value: &Value,
    list: Option<&super::model::ListMetadata>,
) -> bool {
    match data_type {
        ConfigDataType::Password => value.is_string() || value.is_object(),
        ConfigDataType::String
        | ConfigDataType::Key
        | ConfigDataType::PasswordReference
        | ConfigDataType::FilePath
        | ConfigDataType::Executable => value.is_string(),
        ConfigDataType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        ConfigDataType::Number => value.is_number(),
        ConfigDataType::Boolean => value.is_boolean(),
        ConfigDataType::Dictionary | ConfigDataType::Structure => value.is_object(),
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
        processed: std::sync::Mutex<Option<Value>>,
        reject: std::sync::atomic::AtomicBool,
        service: std::sync::OnceLock<std::sync::Weak<ConfigService>>,
    }

    impl ConfigChangeHandler for TestHandler {
        fn process(&self, change: ConfigChange) -> ProcessFuture<'_> {
            Box::pin(async move {
                let value = change.value.clone();
                self.handle(change).await?;
                Ok(self.processed.lock().unwrap().clone().unwrap_or(value))
            })
        }

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

    #[tokio::test]
    async fn stores_and_publishes_processed_list_and_rejects_invalid_output() {
        let fixture = Fixture::new();
        let path = fixture.0.join("config.yml");
        let mut metadata = metadata();
        metadata.items[1].data_type = ConfigDataType::List;
        metadata.items[1].default = Some(json!([1, 1, 2]));
        let handler = Arc::new(TestHandler::default());
        let service = ConfigService::load_with_metadata(&path, vec![metadata.clone()])
            .unwrap()
            .with_change_handler("module_a", handler.clone());
        let mut changes = service.subscribe();

        *handler.processed.lock().unwrap() = Some(json!([1, 3]));
        let saved = service
            .replace("module_a", "config_b", json!([1, 2]))
            .await
            .unwrap();
        assert_eq!(saved, json!([1, 3]));
        let change = changes.try_recv().unwrap();
        assert_eq!(change.operation, ConfigChangeOperation::Remove);
        assert_eq!(change.value, saved);
        assert_eq!(change.module_values["config_b"], saved);
        let reloaded = ConfigService::load_with_metadata(&path, vec![metadata]).unwrap();
        assert_eq!(
            reloaded.value("module_a", "config_b").await.unwrap().value,
            saved
        );

        let disk = fs::read(&path).unwrap();
        *handler.processed.lock().unwrap() = Some(json!("无效的处理结果"));
        assert!(matches!(
            service.partial("module_a", "config_b", json!([2])).await,
            Err(ConfigServiceError::ChangeRejected { .. })
        ));
        assert_eq!(
            handler.seen.lock().unwrap().last().unwrap().operation,
            ConfigChangeOperation::Append
        );
        assert_eq!(
            service.value("module_a", "config_b").await.unwrap().value,
            saved
        );
        assert_eq!(fs::read(&path).unwrap(), disk);
        assert!(changes.try_recv().is_err());
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
