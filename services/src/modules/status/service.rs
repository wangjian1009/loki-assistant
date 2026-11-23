use std::{
    collections::BTreeMap,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use super::model::StatusModuleMetadata;
use super::protocol::{
    API_VERSION, StatusModuleResponse, StatusPathResponse, StatusResponse, StatusValueResponse,
    StatusValues,
};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("状态名称必须非空，不能包含 /，也不能是 . 或 ..")]
    InvalidName,
    #[error("状态发布必须包含至少一个模块和数据项")]
    EmptyUpdate,
    #[error("未声明状态数据结构：{module_id}/{key}")]
    SchemaNotFound { module_id: String, key: String },
    #[error("状态数据无效：{path}：{reason}")]
    InvalidValue { path: String, reason: String },
    #[error("尚未收到状态信息：{0}/{1}")]
    NotFound(String, String),
    #[error("状态项 {0}/{1} 仅限本机访问")]
    RemoteAccessForbidden(String, String),
}

#[derive(Debug, Default)]
struct Snapshot {
    values: StatusValues,
    timestamps: BTreeMap<(String, String), u64>,
    updated_at: Option<u64>,
}

/// 通用内存状态存储；数据由发布方提供，不查询业务模块，不保存到配置文件。
#[derive(Debug, Default)]
pub struct StatusService {
    snapshot: RwLock<Snapshot>,
}

impl StatusService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn metadata(&self) -> Vec<StatusModuleMetadata> {
        super::registry::metadata()
    }

    pub(super) fn metadata_for(&self, remote: bool) -> Vec<StatusModuleMetadata> {
        let mut modules = self.metadata();
        if remote {
            for module in &mut modules {
                module.items.retain(|item| item.remote_access);
            }
            modules.retain(|module| !module.items.is_empty());
        }
        modules
    }

    fn valid_name(name: &str) -> Result<(), StatusError> {
        if name.trim().is_empty() || name.contains('/') || matches!(name, "." | "..") {
            Err(StatusError::InvalidName)
        } else {
            Ok(())
        }
    }

    pub fn snapshot(&self) -> StatusResponse {
        self.snapshot_for(false)
    }

    pub(super) fn snapshot_for(&self, remote: bool) -> StatusResponse {
        let metadata = remote.then(|| self.metadata());
        let state = self.snapshot.read().expect("状态锁不应中毒");
        Self::snapshot_from(&state, metadata.as_deref())
    }

    fn snapshot_from(
        state: &Snapshot,
        metadata: Option<&[StatusModuleMetadata]>,
    ) -> StatusResponse {
        let (status, updated_at) = match metadata {
            None => (state.values.clone(), state.updated_at),
            Some(metadata) => {
                let mut updated_at = None;
                let status = state
                    .values
                    .iter()
                    .filter_map(|(module_id, values)| {
                        let (visible, module_updated_at) =
                            Self::visible_module_values(state, module_id, values, Some(metadata));
                        updated_at = updated_at.max(module_updated_at);
                        (!visible.is_empty()).then(|| (module_id.clone(), visible))
                    })
                    .collect();
                (status, updated_at)
            }
        };
        StatusResponse {
            api_version: API_VERSION.to_owned(),
            status,
            updated_at,
        }
    }

    fn visible_module_values(
        state: &Snapshot,
        module_id: &str,
        values: &BTreeMap<String, Value>,
        metadata: Option<&[StatusModuleMetadata]>,
    ) -> (BTreeMap<String, Value>, Option<u64>) {
        let mut updated_at = None;
        let visible = values
            .iter()
            .filter(|(key, _)| {
                metadata.is_none_or(|metadata| Self::allows_remote(metadata, module_id, key))
            })
            .map(|(key, value)| {
                let timestamp = state.timestamps[&(module_id.to_owned(), key.clone())];
                updated_at = updated_at.max(Some(timestamp));
                (key.clone(), value.clone())
            })
            .collect();
        (visible, updated_at)
    }

    fn allows_remote(metadata: &[StatusModuleMetadata], module_id: &str, key: &str) -> bool {
        metadata
            .iter()
            .find(|module| module.id == module_id)
            .and_then(|module| module.items.iter().find(|item| item.key == key))
            .is_some_and(|item| item.remote_access)
    }

    fn check_access(&self, module_id: &str, key: &str, remote: bool) -> Result<(), StatusError> {
        Self::valid_name(module_id)?;
        Self::valid_name(key)?;
        if remote && !Self::allows_remote(&self.metadata(), module_id, key) {
            return Err(StatusError::RemoteAccessForbidden(
                module_id.to_owned(),
                key.to_owned(),
            ));
        }
        Ok(())
    }

    pub fn value(&self, module_id: &str, key: &str) -> Result<StatusValueResponse, StatusError> {
        self.value_for(module_id, key, false)
    }

    pub(crate) fn value_for(
        &self,
        module_id: &str,
        key: &str,
        remote: bool,
    ) -> Result<StatusValueResponse, StatusError> {
        self.check_access(module_id, key, remote)?;
        let state = self.snapshot.read().expect("状态锁不应中毒");
        let value = state
            .values
            .get(module_id)
            .and_then(|values| values.get(key))
            .ok_or_else(|| StatusError::NotFound(module_id.to_owned(), key.to_owned()))?;
        Ok(StatusValueResponse {
            api_version: API_VERSION.to_owned(),
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            value: value.clone(),
            updated_at: state.timestamps[&(module_id.to_owned(), key.to_owned())],
        })
    }

    pub fn module_value(&self, module_id: &str) -> Result<StatusModuleResponse, StatusError> {
        self.module_value_for(module_id, false)
    }

    pub(super) fn module_value_for(
        &self,
        module_id: &str,
        remote: bool,
    ) -> Result<StatusModuleResponse, StatusError> {
        Self::valid_name(module_id)?;
        let metadata = remote.then(|| self.metadata());
        let state = self.snapshot.read().expect("状态锁不应中毒");
        let values = state
            .values
            .get(module_id)
            .ok_or_else(|| StatusError::NotFound(module_id.to_owned(), String::new()))?;
        let (value, updated_at) =
            Self::visible_module_values(&state, module_id, values, metadata.as_deref());
        let updated_at =
            updated_at.ok_or_else(|| StatusError::NotFound(module_id.to_owned(), String::new()))?;
        Ok(StatusModuleResponse {
            api_version: API_VERSION.to_owned(),
            module_id: module_id.to_owned(),
            value,
            updated_at,
        })
    }

    pub fn path_value(
        &self,
        module_id: &str,
        key: &str,
        path: &str,
    ) -> Result<StatusPathResponse, StatusError> {
        self.path_value_for(module_id, key, path, false)
    }

    pub(super) fn path_value_for(
        &self,
        module_id: &str,
        key: &str,
        path: &str,
        remote: bool,
    ) -> Result<StatusPathResponse, StatusError> {
        self.check_access(module_id, key, remote)?;
        let segments: Vec<_> = path.split('/').collect();
        if segments.is_empty() || segments.iter().any(|segment| segment.is_empty()) {
            return Err(StatusError::InvalidName);
        }
        for segment in &segments {
            Self::valid_name(segment)?;
        }
        let state = self.snapshot.read().expect("状态锁不应中毒");
        let root = state
            .values
            .get(module_id)
            .and_then(|values| values.get(key))
            .ok_or_else(|| StatusError::NotFound(module_id.to_owned(), key.to_owned()))?;
        let mut value = root;
        for segment in &segments {
            value = match value {
                Value::Object(object) => object.get(*segment),
                Value::Array(array) => segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| array.get(index)),
                _ => None,
            }
            .ok_or_else(|| StatusError::NotFound(module_id.to_owned(), format!("{key}/{path}")))?;
        }
        Ok(StatusPathResponse {
            api_version: API_VERSION.to_owned(),
            module_id: module_id.to_owned(),
            path: format!("{key}/{path}"),
            value: value.clone(),
            updated_at: state.timestamps[&(module_id.to_owned(), key.to_owned())],
        })
    }

    pub fn publish(
        &self,
        module_id: &str,
        key: &str,
        value: Value,
    ) -> Result<StatusValueResponse, StatusError> {
        Self::valid_name(module_id)?;
        Self::valid_name(key)?;
        let mut state = self.snapshot.write().expect("状态锁不应中毒");
        let updated_at = Self::now();
        Self::put(
            &mut state,
            module_id.to_owned(),
            key.to_owned(),
            value.clone(),
            updated_at,
        );
        Ok(StatusValueResponse {
            api_version: API_VERSION.to_owned(),
            module_id: module_id.to_owned(),
            key: key.to_owned(),
            value,
            updated_at,
        })
    }

    pub fn publish_many(&self, values: StatusValues) -> Result<StatusResponse, StatusError> {
        Self::validate_names(&values)?;
        Ok(self.publish_batch(values))
    }

    fn publish_batch(&self, values: StatusValues) -> StatusResponse {
        let mut state = self.snapshot.write().expect("状态锁不应中毒");
        let now = Self::now();
        for (module, items) in values {
            for (key, value) in items {
                Self::put(&mut state, module.clone(), key, value, now);
            }
        }
        Self::snapshot_from(&state, None)
    }

    /// 外部上报使用声明结构严格校验，整批通过后才更新共享状态。
    pub fn publish_many_validated(
        &self,
        values: StatusValues,
    ) -> Result<StatusResponse, StatusError> {
        Self::validate_names(&values)?;
        let metadata = self.metadata();
        for (module_id, items) in &values {
            let module = metadata.iter().find(|module| &module.id == module_id);
            for (key, value) in items {
                let item = module
                    .and_then(|module| module.items.iter().find(|item| &item.key == key))
                    .ok_or_else(|| StatusError::SchemaNotFound {
                        module_id: module_id.clone(),
                        key: key.clone(),
                    })?;
                super::validation::validate(&item.schema, value, &format!("{module_id}/{key}"))?;
            }
        }
        Ok(self.publish_batch(values))
    }

    fn validate_names(values: &StatusValues) -> Result<(), StatusError> {
        if values.is_empty() || values.values().any(|items| items.is_empty()) {
            return Err(StatusError::EmptyUpdate);
        }
        for (module, items) in values {
            Self::valid_name(module)?;
            for key in items.keys() {
                Self::valid_name(key)?;
            }
        }
        Ok(())
    }

    fn put(state: &mut Snapshot, module: String, key: String, value: Value, now: u64) {
        state.timestamps.insert((module.clone(), key.clone()), now);
        state.values.entry(module).or_default().insert(key, value);
        state.updated_at = Some(now);
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}
