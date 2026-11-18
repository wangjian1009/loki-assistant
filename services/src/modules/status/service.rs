use std::{
    collections::BTreeMap,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use super::model::StatusModuleMetadata;
use super::protocol::{API_VERSION, StatusResponse, StatusValueResponse, StatusValues};
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

    fn valid_name(name: &str) -> Result<(), StatusError> {
        if name.trim().is_empty() || name.contains('/') || matches!(name, "." | "..") {
            Err(StatusError::InvalidName)
        } else {
            Ok(())
        }
    }

    pub fn snapshot(&self) -> StatusResponse {
        let state = self.snapshot.read().expect("状态锁不应中毒");
        StatusResponse {
            api_version: API_VERSION.to_owned(),
            status: state.values.clone(),
            updated_at: state.updated_at,
        }
    }

    pub fn value(&self, module_id: &str, key: &str) -> Result<StatusValueResponse, StatusError> {
        Self::valid_name(module_id)?;
        Self::valid_name(key)?;
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
        let mut state = self.snapshot.write().expect("状态锁不应中毒");
        let now = Self::now();
        for (module, items) in values {
            for (key, value) in items {
                Self::put(&mut state, module.clone(), key, value, now);
            }
        }
        Ok(StatusResponse {
            api_version: API_VERSION.to_owned(),
            status: state.values.clone(),
            updated_at: state.updated_at,
        })
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
        self.publish_many(values)
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
