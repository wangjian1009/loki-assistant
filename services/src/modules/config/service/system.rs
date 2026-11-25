use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ConfigService, ConfigServiceError, StoredConfig, StoredValues};
use crate::modules::password::service::{PasswordError, SYSTEM_PASSWORD_PREFIX, validate_file};

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
        let published = self.values.clone();
        self.data
            .call_async(move |mut data| async move {
                let result = data.write_snapshot(&values, &data.system).await;
                if result.is_ok() {
                    data.values = std::sync::Arc::new(values);
                    let next = data.values.clone();
                    published.call(move |current| *current = next).await;
                }
                (data, result)
            })
            .await
    }

    pub(crate) async fn system_value(&self, key: &str) -> Option<Value> {
        let key = key.to_owned();
        self.data
            .call(move |data| data.system.0.get(&key).cloned())
            .await
    }

    // 提交前处理器可以独立更新内部值。
    pub(crate) async fn update_system_value<
        R: Send + 'static,
        F: std::future::Future<Output = Result<(Option<Value>, R), ConfigServiceError>>
            + Send
            + 'static,
    >(
        &self,
        key: &str,
        update: impl FnOnce(Option<Value>) -> F + Send + 'static,
    ) -> Result<R, ConfigServiceError> {
        let key = key.to_owned();
        self.data
            .call_async(move |mut data| async move {
                let result = data.update_system_value(&key, update).await;
                (data, result)
            })
            .await
    }
}

impl StoredConfig {
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

    async fn update_system_value<
        R,
        F: std::future::Future<Output = Result<(Option<Value>, R), ConfigServiceError>>,
    >(
        &mut self,
        key: &str,
        update: impl FnOnce(Option<Value>) -> F,
    ) -> Result<R, ConfigServiceError> {
        let mut system = self.system.clone();
        let previous = system.0.get(key).cloned();
        let (value, result) = update(previous.clone()).await?;
        if value != previous {
            match value {
                Some(value) => system.0.insert(key.to_owned(), value),
                None => system.0.remove(key),
            };
            self.write_snapshot(&self.values, &system).await?;
            self.system = system;
        }
        Ok(result)
    }
}

impl ConfigService {
    #[cfg(test)]
    pub(crate) async fn set_system_value(
        &self,
        key: &str,
        value: impl Serialize + Send,
    ) -> Result<(), ConfigServiceError> {
        let value = serde_json::to_value(value)
            .map_err(|_| ConfigServiceError::Storage("内部配置序列化失败。".into()))?;
        self.update_system_value(key, async move |_| Ok((Some(value), ())))
            .await
    }
}

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

    /// 已写入且内容一致的条目可以复用。
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
