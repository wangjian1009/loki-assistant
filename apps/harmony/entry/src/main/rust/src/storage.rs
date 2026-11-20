use crate::network::PAIR_ALPN;
use iroh::{EndpointId, SecretKey};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Invitation {
    pub(crate) version: u32,
    pub(crate) pairing_id: String,
    pub(crate) endpoint_id: String,
    pub(crate) relay_url: String,
    pub(crate) relay_token: String,
    pub(crate) alpn: String,
    pub(crate) pairing_token: String,
    pub(crate) expires_at: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Computer {
    pub(crate) endpoint_id: String,
    pub(crate) relay_url: String,
    pub(crate) relay_token: String,
    pub(crate) bound_at: Option<u64>,
    pub(crate) pending: Option<Invitation>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selected_endpoint_id: Option<String>,
    pub(crate) version: u32,
    pub(crate) secret_key: String,
    pub(crate) endpoint_id: String,
    pub(crate) computers: Vec<Computer>,
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn valid_relay(address: &str, token: &str) -> bool {
    url::Url::parse(address).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    }) && !token.trim().is_empty()
        && token.len() <= 4096
}

pub(crate) fn parse_invitation(payload: &str) -> Result<Invitation> {
    let invalid = || "这不是有效的 Loki 设备绑定二维码".to_owned();
    if payload.len() > 16_384 {
        return Err(invalid());
    }
    let value: Invitation = serde_json::from_str(payload).map_err(|_| invalid())?;
    if value.version != 1
        || value.alpn.as_bytes() != PAIR_ALPN
        || value.endpoint_id.parse::<EndpointId>().is_err()
        || uuid::Uuid::parse_str(&value.pairing_id).is_err()
        || value.pairing_token.len() != 64
        || !value
            .pairing_token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !valid_relay(&value.relay_url, &value.relay_token)
    {
        return Err(invalid());
    }
    if value.expires_at <= now() {
        return Err("二维码已过期，请在电脑上重新生成".into());
    }
    Ok(value)
}

pub(crate) fn save(path: &Path, store: &Store) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let data = serde_json::to_vec(store)?;
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
    write().map_err(|_| "无法保存设备记录，请检查应用存储空间".into())
}

pub(crate) fn load(path: &Path) -> Result<Store> {
    let storage_error = || "设备记录无法读取或已损坏，请勿清除应用数据以免丢失身份".to_owned();
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => {
            if !file.metadata().map_err(|_| storage_error())?.is_file() {
                return Err(storage_error());
            }
            let mut bytes = Vec::new();
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|_| storage_error())?;
            if bytes.len() > 1_048_576 {
                return Err(storage_error());
            }
            let store: Store = serde_json::from_slice(&bytes).map_err(|_| storage_error())?;
            let key: SecretKey = store.secret_key.parse().map_err(|_| storage_error())?;
            let mut ids = std::collections::HashSet::new();
            if store.version != 1
                || key.public().to_string() != store.endpoint_id
                || store.computers.len() > 64
                || store
                    .selected_endpoint_id
                    .as_ref()
                    .is_some_and(|id| !store.computers.iter().any(|c| &c.endpoint_id == id))
                || store.computers.iter().any(|computer| {
                    computer.endpoint_id.parse::<EndpointId>().is_err()
                        || computer.endpoint_id == store.endpoint_id
                        || !ids.insert(&computer.endpoint_id)
                        || !valid_relay(&computer.relay_url, &computer.relay_token)
                        || (computer.bound_at.is_none() && computer.pending.is_none())
                        || computer.pending.as_ref().is_some_and(|pending| {
                            pending.endpoint_id != computer.endpoint_id
                                || pending.relay_url != computer.relay_url
                                || pending.relay_token != computer.relay_token
                        })
                })
            {
                return Err(storage_error());
            }
            Ok(store)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let store = Store {
                selected_endpoint_id: None,
                version: 1,
                secret_key: key
                    .to_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                endpoint_id: key.public().to_string(),
                computers: vec![],
            };
            save(path, &store)?;
            Ok(store)
        }
        Err(_) => Err(storage_error()),
    }
}
