use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::modules::config::service::{
    ConfigService, ConfigServiceError, LegacySystemFile, Password, SYSTEM_PASSWORD_PREFIX,
};

use super::protocol::{BindingToken, DeviceType, NetworkError, Peer};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound = "T: Serialize + serde::de::DeserializeOwned"
)]
pub(super) struct PeerRecord<T> {
    #[serde(default)]
    pub device_type: DeviceType,
    pub endpoint_id: String,
    pub name: String,
    pub bound_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_token: Option<T>,
}
pub(super) type StoredPeer = PeerRecord<BindingToken>;

impl<T> PeerRecord<T> {
    fn with_password<U>(self, binding_token: Option<U>) -> PeerRecord<U> {
        PeerRecord {
            device_type: self.device_type,
            endpoint_id: self.endpoint_id,
            name: self.name,
            bound_at: self.bound_at,
            binding_token,
        }
    }
}

impl StoredPeer {
    pub fn public(&self) -> Peer {
        Peer {
            device_type: self.device_type,
            endpoint_id: self.endpoint_id.clone(),
            name: self.name.clone(),
            bound_at: self.bound_at,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound = "T: Serialize + serde::de::DeserializeOwned"
)]
struct StoredPeers<T> {
    version: u32,
    peers: Vec<PeerRecord<T>>,
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn invalid() -> ConfigServiceError {
    ConfigServiceError::Storage("设备绑定记录格式无效。".into())
}

impl<T> StoredPeers<T> {
    fn validate(&self) -> Result<(), ConfigServiceError> {
        if self.peers.len() > 256 {
            return Err(invalid());
        }
        let mut ids = HashSet::new();
        for peer in &self.peers {
            let id = peer
                .endpoint_id
                .parse::<iroh::EndpointId>()
                .map_err(|_| invalid())?;
            if id.to_string() != peer.endpoint_id
                || !valid_name(&peer.name)
                || peer.bound_at == 0
                || !ids.insert(id)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

const KEY: &str = "network_peers";

fn valid_token(token: &str) -> bool {
    token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn password_path(endpoint_id: &str, token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{SYSTEM_PASSWORD_PREFIX}peers/{endpoint_id}/{digest:x}.gpg")
}

fn decode<T: serde::de::DeserializeOwned + Serialize>(
    value: Value,
    version: u32,
) -> Result<StoredPeers<T>, ConfigServiceError> {
    let peers: StoredPeers<T> = serde_json::from_value(value).map_err(|_| invalid())?;
    if peers.version != version {
        return Err(invalid());
    }
    peers.validate()?;
    Ok(peers)
}

async fn to_passwords(
    config: &ConfigService,
    peers: &[StoredPeer],
    existing: &HashSet<String>,
) -> Result<StoredPeers<Password>, ConfigServiceError> {
    let mut records = Vec::with_capacity(peers.len());
    for peer in peers {
        let password = match &peer.binding_token {
            Some(token) if valid_token(&token.0) => {
                let password = Password::new(password_path(&peer.endpoint_id, &token.0));
                if !existing.contains(password.path()) {
                    password.import(config, &token.0).await?;
                }
                Some(password)
            }
            Some(_) => return Err(invalid()),
            None => None,
        };
        records.push(peer.clone().with_password(password));
    }
    Ok(StoredPeers {
        version: 2,
        peers: records,
    })
}

async fn from_passwords(
    config: &ConfigService,
    records: StoredPeers<Password>,
) -> Result<Vec<StoredPeer>, ConfigServiceError> {
    let mut peers = Vec::with_capacity(records.peers.len());
    for record in records.peers {
        let token = match &record.binding_token {
            Some(password) => {
                let prefix = format!("{SYSTEM_PASSWORD_PREFIX}peers/{}/", record.endpoint_id);
                if !password
                    .path()
                    .strip_prefix(&prefix)
                    .and_then(|suffix| suffix.strip_suffix(".gpg"))
                    .is_some_and(valid_token)
                {
                    return Err(invalid());
                }
                let token = password.read(config).await?;
                if !valid_token(&token)
                    || password.path() != password_path(&record.endpoint_id, &token)
                {
                    return Err(invalid());
                }
                Some(BindingToken(token))
            }
            None => None,
        };
        peers.push(record.with_password(token));
    }
    Ok(peers)
}

pub(super) async fn load(
    config: &ConfigService,
) -> Result<BTreeMap<String, StoredPeer>, NetworkError> {
    let peers = config
        .update_system_value(
            KEY,
            Some(LegacySystemFile {
                name: "peers.json",
                limit: 1_048_576,
            }),
            async |previous| {
                let Some(value) = previous else {
                    return Ok((None, Vec::new()));
                };
                if value["version"] == 2 {
                    let peers = from_passwords(config, decode(value.clone(), 2)?).await?;
                    return Ok((Some(value), peers));
                }
                let peers = decode(value, 1)?.peers;
                let records = to_passwords(config, &peers, &HashSet::new()).await?;
                Ok((
                    Some(serde_json::to_value(records).map_err(|_| invalid())?),
                    peers,
                ))
            },
        )
        .await
        .map_err(|_| NetworkError::Storage)?;
    Ok(peers
        .into_iter()
        .map(|peer| (peer.endpoint_id.clone(), peer))
        .collect())
}

pub(super) async fn save(
    config: &ConfigService,
    peers: Vec<StoredPeer>,
) -> Result<(), NetworkError> {
    let retired = config
        .update_system_value(KEY, None, async |previous| {
            let existing = previous
                .map(|value| decode::<Password>(value, 2))
                .transpose()?
                .into_iter()
                .flat_map(|records| records.peers)
                .filter_map(|record| record.binding_token)
                .map(|password| password.path().to_owned())
                .collect();
            let records = to_passwords(config, &peers, &existing).await?;
            let retained: HashSet<_> = records
                .peers
                .iter()
                .filter_map(|record| record.binding_token.as_ref())
                .map(Password::path)
                .collect();
            let retired: Vec<_> = existing
                .iter()
                .filter(|path| !retained.contains(path.as_str()))
                .map(|path| Password::new(path.clone()))
                .collect();
            Ok((
                Some(serde_json::to_value(records).map_err(|_| invalid())?),
                retired,
            ))
        })
        .await
        .map_err(|_| NetworkError::Storage)?;
    for password in retired {
        if let Err(error) = password.remove(config).await {
            eprintln!("清理旧设备绑定凭据失败：{error}");
        }
    }
    Ok(())
}
