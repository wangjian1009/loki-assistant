use loki_services_macros::loki_config;
use serde_json::Value;

use crate::modules::config::{
    model::{ConfigDataType, ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata},
    registry::ConfigItemRegistration,
    service::{ConfigService, ConfigServiceError, Password},
};

const RELAY_PASSWORD_PATH: &str = "loki-assistant/network/relay.gpg";
const LEGACY_RELAY_TOKEN: &str = "network_relay_token";

pub(super) fn relay_password() -> Password {
    Password::new(RELAY_PASSWORD_PATH.into())
}

pub(super) async fn relay_token(config: &ConfigService) -> Result<String, ConfigServiceError> {
    let password = relay_password();
    if config.system_value(LEGACY_RELAY_TOKEN).await.is_some() {
        let storage = config.clone();
        config
            .update_system_value(LEGACY_RELAY_TOKEN, None, async move |legacy| {
                if let Some(value) = legacy {
                    let value = value.as_str().ok_or_else(|| {
                        ConfigServiceError::Storage("旧中转令牌格式无效。".into())
                    })?;
                    if !value.trim().is_empty() {
                        password.import(&storage, value).await?;
                    }
                }
                Ok((None, ()))
            })
            .await?;
    }
    Ok(relay_password()
        .read_optional(config)
        .await?
        .unwrap_or_default())
}

pub(super) async fn relay_token_is_set(config: &ConfigService) -> bool {
    if let Some(legacy) = config.system_value(LEGACY_RELAY_TOKEN).await
        && legacy
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    {
        return true;
    }
    relay_password()
        .read_optional(config)
        .await
        .ok()
        .flatten()
        .is_some_and(|value| !value.trim().is_empty())
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static RELAY_TOKEN_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "network",
    key: "relay_token",
    options: || ConfigItemOptions {
        read_only: true,
        ..ConfigItemOptions::default()
    },
};

#[loki_config]
fn network_config_metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "network".to_owned(),
        name: "网络".to_owned(),
        description: "管理本地身份和中转服务器注册。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![
            ConfigItemMetadata {
                key: "relay_url".to_owned(),
                name: "中转地址".to_owned(),
                description:
                    "中转服务器的完整 HTTP(S) URL；地址和令牌都非空时注册，缺少任意一项视为未配置。"
                        .to_owned(),
                data_type: ConfigDataType::String,
                remote_access: crate::modules::config::model::ConfigRemoteAccess::Read,
                required: false,
                default: Some(Value::String(String::new())),
                dictionary: None,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
            ConfigItemMetadata {
                key: "relay_token".to_owned(),
                name: "中转令牌".to_owned(),
                description: "保存在本机 GPG 密码库中的共享访问令牌，与中转地址一起生效。"
                    .to_owned(),
                data_type: ConfigDataType::Password {
                    path: RELAY_PASSWORD_PATH.into(),
                    key: None,
                },
                remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                required: false,
                default: None,
                dictionary: None,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
        ],
    }
}
