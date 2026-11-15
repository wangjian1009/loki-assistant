use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::{Map, Value};

use super::service::ConfigServiceError;

pub(super) type StoredValues = HashMap<String, Map<String, Value>>;

#[derive(Clone, Debug)]
pub(super) struct FileStorage {
    path: PathBuf,
}

impl FileStorage {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, ConfigServiceError> {
        Ok(Self {
            path: std::path::absolute(path).map_err(storage_error)?,
        })
    }

    pub fn read(&self) -> Result<StoredValues, ConfigServiceError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(StoredValues::new()),
            Err(error) => return Err(storage_error(error)),
        };
        serde_yaml_ng::from_str::<Option<StoredValues>>(&text)
            .map(Option::unwrap_or_default)
            .map_err(|error| {
                ConfigServiceError::Storage(format!("读取 {} 失败：{error}", self.path.display()))
            })
    }

    pub async fn write(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        let storage = self.clone();
        tokio::task::spawn_blocking(move || storage.write_atomic(values))
            .await
            .map_err(storage_error)?
    }

    fn write_atomic(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let parent = self.path.parent().expect("绝对配置路径应包含父目录");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(storage_error)?;
        let temporary = parent.join(format!(
            ".config.{}.{}.tmp",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let text = serde_yaml_ng::to_string(&values.into_iter().collect::<BTreeMap<_, _>>())
            .map_err(storage_error)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(storage_error)?;
        let result = (|| {
            file.write_all(text.as_bytes()).map_err(storage_error)?;
            file.sync_all().map_err(storage_error)?;
            fs::rename(&temporary, &self.path).map_err(storage_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn storage_error(error: impl std::fmt::Display) -> ConfigServiceError {
    ConfigServiceError::Storage(error.to_string())
}
