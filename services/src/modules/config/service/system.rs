use std::{
    fmt,
    fs::{self, File},
    io::{ErrorKind, Read},
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ConfigService, ConfigServiceError, StoredValues};
use crate::modules::{
    config::storage::open_private_file,
    password::service::{PasswordError, validate_file},
};

pub(super) const SECTION: &str = "_system";

#[derive(Clone, Default)]
pub(super) struct SystemValues(pub Map<String, Value>);

impl fmt::Debug for SystemValues {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SystemValues(..)")
    }
}

impl ConfigService {
    pub(super) async fn persist_values(
        &self,
        values: StoredValues,
    ) -> Result<(), ConfigServiceError> {
        let _persistence = self.persistence.lock().await;
        self.write_snapshot(&values, &*self.system_values.read().await)
            .await?;
        *self.values.write().await = values;
        Ok(())
    }

    async fn write_snapshot(
        &self,
        values: &StoredValues,
        system: &SystemValues,
    ) -> Result<(), ConfigServiceError> {
        if let Some(storage) = &self.storage {
            let mut snapshot = values.clone();
            if !system.0.is_empty() {
                snapshot.insert(SECTION.to_owned(), system.0.clone());
            }
            storage.write(snapshot).await?;
        }
        Ok(())
    }

    async fn write_system(&self, system: SystemValues) -> Result<(), ConfigServiceError> {
        self.write_snapshot(&*self.values.read().await, &system)
            .await?;
        *self.system_values.write().await = system;
        Ok(())
    }

    pub(crate) async fn system_value(&self, key: &str) -> Option<Value> {
        self.system_values.read().await.0.get(key).cloned()
    }

    // 处理器可更新内部值，落盘锁独立于配置提交锁。
    pub(crate) async fn update_system_value<R>(
        &self,
        key: &str,
        legacy: Option<LegacySystemFile>,
        update: impl AsyncFnOnce(Option<Value>) -> Result<(Option<Value>, R), ConfigServiceError>,
    ) -> Result<R, ConfigServiceError> {
        let _persistence = self.persistence.lock().await;
        let mut system = self.system_values.read().await.clone();
        let previous = system.0.get(key).cloned();
        let mut source = None;
        let mut value = previous.clone();
        if value.is_none()
            && let Some(legacy) = legacy
            && let Some(storage) = &self.storage
        {
            let path = storage.sibling(legacy.name);
            (source, value) = tokio::task::spawn_blocking(move || {
                let value = read_legacy(&path, legacy.limit)?;
                Ok::<_, ConfigServiceError>((value.as_ref().map(|_| path), value))
            })
            .await
            .map_err(task_error)??;
        }
        let (value, result) = update(value).await?;
        if value != previous {
            match value {
                Some(value) => system.0.insert(key.to_owned(), value),
                None => system.0.remove(key),
            };
            self.write_system(system).await?;
        }
        if let Some(path) = source
            && let Err(error) = tokio::task::spawn_blocking(move || {
                File::open(path.parent().expect("旧配置文件位于配置目录"))?.sync_all()?;
                fs::remove_file(path)
            })
            .await
            .map_err(task_error)?
        {
            eprintln!("内部配置已迁移，清理旧文件失败：{error}");
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) async fn set_system_value(
        &self,
        key: &str,
        value: impl Serialize + Send,
    ) -> Result<(), ConfigServiceError> {
        let value = serde_json::to_value(value)
            .map_err(|_| ConfigServiceError::Storage("内部配置序列化失败。".into()))?;
        self.update_system_value(key, None, async |_| Ok((Some(value), ())))
            .await
    }
}

pub(crate) struct LegacySystemFile {
    pub name: &'static str,
    pub limit: u64,
}

pub(crate) const SYSTEM_PASSWORD_PREFIX: &str = "loki-assistant/network/";

/// 内部配置中保存的 Password 条目引用。
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Password(String);

impl Password {
    pub(crate) fn new(path: String) -> Self {
        Self(path)
    }

    pub(crate) fn path(&self) -> &str {
        &self.0
    }

    pub(crate) async fn read_optional(
        &self,
        config: &ConfigService,
    ) -> Result<Option<String>, ConfigServiceError> {
        self.validate()?;
        Ok(config
            .load_password_entry(&self.0)
            .await?
            .map(|contents| contents.split('\n').next().unwrap_or_default().to_owned()))
    }

    pub(crate) async fn read(&self, config: &ConfigService) -> Result<String, ConfigServiceError> {
        self.read_optional(config)
            .await?
            .ok_or_else(|| password_error(PasswordError::EntryNotFound))
    }

    pub(crate) async fn write(
        &self,
        config: &ConfigService,
        value: &str,
    ) -> Result<(), ConfigServiceError> {
        self.validate()?;
        config
            .passwords()?
            .save(&self.0, value)
            .await
            .map_err(password_error)
    }

    pub(crate) async fn remove(&self, config: &ConfigService) -> Result<(), ConfigServiceError> {
        self.validate()?;
        config
            .passwords()?
            .remove(&self.0)
            .await
            .map_err(password_error)
    }

    /// 重试迁移时复用已写入且内容一致的条目。
    pub(crate) async fn import(
        &self,
        config: &ConfigService,
        value: &str,
    ) -> Result<(), ConfigServiceError> {
        match self.read_optional(config).await? {
            Some(existing) if existing == value => Ok(()),
            Some(_) => Err(ConfigServiceError::Storage(
                "旧凭据与密码库条目不一致。".into(),
            )),
            None => {
                self.write(config, value).await?;
                if self.read_optional(config).await?.as_deref() != Some(value) {
                    return Err(ConfigServiceError::Storage(
                        "无法核对已写入的密码条目。".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn validate(&self) -> Result<(), ConfigServiceError> {
        if !self.0.starts_with(SYSTEM_PASSWORD_PREFIX) {
            return Err(password_error(PasswordError::InvalidPath));
        }
        validate_file(&self.0).map_err(password_error)
    }
}

fn password_error(error: PasswordError) -> ConfigServiceError {
    ConfigServiceError::Storage(error.to_string())
}

fn read_legacy(path: &Path, limit: u64) -> Result<Option<Value>, ConfigServiceError> {
    let file = match open_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigServiceError::Storage(error.to_string())),
    };
    let mut data = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|error| ConfigServiceError::Storage(error.to_string()))?;
    let invalid = || ConfigServiceError::Storage("旧配置文件过大或格式无效。".into());
    if data.len() as u64 > limit {
        return Err(invalid());
    }
    let value = serde_json::from_slice(&data).map_err(|_| invalid())?;
    Ok(Some(value))
}

fn task_error(error: tokio::task::JoinError) -> ConfigServiceError {
    ConfigServiceError::Storage(error.to_string())
}
