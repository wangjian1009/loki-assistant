use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
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
        let mut file = match open_private_file(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(StoredValues::new()),
            Err(error) => return Err(storage_error(error)),
        };
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(storage_error)?;
        serde_yaml_ng::from_str::<Option<StoredValues>>(&text)
            .map(Option::unwrap_or_default)
            .map_err(|error| {
                let location = error.location().map_or_else(String::new, |location| {
                    format!("（第 {} 行，第 {} 列）", location.line(), location.column())
                });
                ConfigServiceError::Storage(format!(
                    "配置文件 {} 格式无效{location}。",
                    self.path.display()
                ))
            })
    }

    pub async fn write(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        let storage = self.clone();
        tokio::task::spawn_blocking(move || storage.write_atomic(values))
            .await
            .map_err(storage_error)?
    }

    pub(super) fn write_atomic(&self, values: StoredValues) -> Result<(), ConfigServiceError> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let parent = self.path.parent().expect("绝对配置路径应包含父目录");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(storage_error)?;
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(storage_error("配置文件必须是普通文件。"));
            }
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(storage_error(error)),
            _ => {}
        }
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

pub(super) fn open_private_file(path: &Path) -> io::Result<fs::File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("配置文件必须是普通文件。"));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn storage_error(error: impl std::fmt::Display) -> ConfigServiceError {
    ConfigServiceError::Storage(error.to_string())
}
