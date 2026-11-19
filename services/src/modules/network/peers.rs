use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::protocol::{NetworkError, Peer};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPeers {
    version: u32,
    peers: Vec<Peer>,
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

pub(super) fn load(path: &Path) -> Result<BTreeMap<String, Peer>, NetworkError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => return Err(NetworkError::Storage),
    };
    if !file
        .metadata()
        .map_err(|_| NetworkError::Storage)?
        .is_file()
    {
        return Err(NetworkError::Storage);
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| NetworkError::Storage)?;
    let mut data = Vec::new();
    Read::by_ref(&mut file)
        .take(1_048_577)
        .read_to_end(&mut data)
        .map_err(|_| NetworkError::Storage)?;
    if data.len() > 1_048_576 {
        return Err(NetworkError::Storage);
    }
    let stored: StoredPeers = serde_json::from_slice(&data).map_err(|_| NetworkError::Storage)?;
    if stored.version != 1 || stored.peers.len() > 256 {
        return Err(NetworkError::Storage);
    }
    let mut peers = BTreeMap::new();
    for peer in stored.peers {
        let id = peer
            .endpoint_id
            .parse::<iroh::EndpointId>()
            .map_err(|_| NetworkError::Storage)?;
        if id.to_string() != peer.endpoint_id
            || !valid_name(&peer.name)
            || peer.bound_at == 0
            || peers.insert(peer.endpoint_id.clone(), peer).is_some()
        {
            return Err(NetworkError::Storage);
        }
    }
    Ok(peers)
}

pub(super) fn save(path: PathBuf, peers: Vec<Peer>) -> Result<(), NetworkError> {
    let parent = path.parent().ok_or(NetworkError::Storage)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| NetworkError::Storage)?;
    // 不替换符号链接或非普通文件；损坏记录只由显式人工修复。
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() => return Err(NetworkError::Storage),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(NetworkError::Storage);
        }
        _ => {}
    }
    let temporary = parent.join(format!(".peers-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| NetworkError::Storage)?;
        let data = serde_json::to_vec_pretty(&StoredPeers { version: 1, peers })
            .map_err(|_| NetworkError::Storage)?;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|_| NetworkError::Storage)?;
        fs::rename(&temporary, &path).map_err(|_| NetworkError::Storage)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
