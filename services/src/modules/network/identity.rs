use crate::modules::password::service::SYSTEM_PASSWORD_PREFIX;
use base64::{Engine, engine::general_purpose::STANDARD};
use iroh::SecretKey;
use serde::{Deserialize, Serialize};

use crate::modules::config::service::{ConfigService, ConfigServiceError, Password};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredIdentity<T> {
    version: u32,
    secret_key: T,
    endpoint_id: String,
}

fn invalid_identity() -> ConfigServiceError {
    ConfigServiceError::Storage("本地身份格式无效或公私钥不匹配。".into())
}

fn validate(key: &SecretKey, endpoint_id: &str) -> Result<(), ConfigServiceError> {
    if key.public().to_string() != endpoint_id {
        return Err(invalid_identity());
    }
    Ok(())
}

fn decode(value: &str) -> Result<SecretKey, ConfigServiceError> {
    let bytes: [u8; 32] = STANDARD
        .decode(value)
        .map_err(|_| invalid_identity())?
        .try_into()
        .map_err(|_| invalid_identity())?;
    Ok(SecretKey::from_bytes(&bytes))
}

pub(super) async fn ensure(config: &ConfigService) -> Result<(SecretKey, bool), String> {
    let config = config.clone();
    tokio::spawn(async move {
        let password = Password::new(format!("{SYSTEM_PASSWORD_PREFIX}identity.gpg"));
        let storage = config.clone();
        config
            .update_system_value("network_identity", async move |previous| {
                let (key, created) = match previous {
                    Some(value) if value["version"] == 2 => {
                        let identity: StoredIdentity<Password> =
                            serde_json::from_value(value.clone())
                                .map_err(|_| invalid_identity())?;
                        if identity.secret_key.path() != password.path() {
                            return Err(invalid_identity());
                        }
                        let key = decode(&identity.secret_key.read(&storage).await?)?;
                        validate(&key, &identity.endpoint_id)?;
                        return Ok((Some(value), (key, false)));
                    }
                    Some(_) => return Err(invalid_identity()),
                    None => match password.read_optional(&storage).await? {
                        Some(value) => (decode(&value)?, false),
                        None => {
                            let key = SecretKey::generate();
                            password
                                .import(&storage, &STANDARD.encode(key.to_bytes()))
                                .await?;
                            (key, true)
                        }
                    },
                };
                let value = serde_json::to_value(StoredIdentity {
                    version: 2,
                    endpoint_id: key.public().to_string(),
                    secret_key: password,
                })
                .map_err(|_| invalid_identity())?;
                Ok((Some(value), (key, created)))
            })
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}
