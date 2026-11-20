use iroh::SecretKey;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
type Result<T> = std::result::Result<T, String>;

/// Rust 只解释本机身份；其余应用数据按原样保存，由应用层解释和管理。
#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Store {
    version: u32,
    pub(crate) secret_key: String,
    pub(crate) endpoint_id: String,
    #[serde(flatten)]
    pub(crate) application: Map<String, Value>,
}

pub(crate) fn save(path: &Path, store: &Store) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let data = serde_json::to_vec(store)?;
        if data.len() > 1_048_576 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()
    };
    write().map_err(|_| "无法保存应用数据，请检查应用存储空间".into())
}

pub(crate) fn load(path: &Path) -> Result<Store> {
    let invalid = || "应用数据无法读取或已损坏，请勿清除应用数据以免丢失身份".to_owned();
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => {
            if !file.metadata().map_err(|_| invalid())?.is_file() {
                return Err(invalid());
            }
            let mut bytes = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid())?;
            if bytes.len() > 1_048_576 {
                return Err(invalid());
            }
            let store: Store = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            let key: SecretKey = store.secret_key.parse().map_err(|_| invalid())?;
            if store.version != 1 || key.public().to_string() != store.endpoint_id {
                return Err(invalid());
            }
            Ok(store)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let store = Store {
                version: 1,
                secret_key: key.to_bytes().iter().map(|b| format!("{b:02x}")).collect(),
                endpoint_id: key.public().to_string(),
                application: Map::new(),
            };
            save(path, &store)?;
            Ok(store)
        }
        Err(_) => Err(invalid()),
    }
}
