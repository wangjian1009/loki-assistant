use loki_services_macros::loki_config;
use serde_json::Value;

use crate::modules::config::{
    model::{ConfigDataType, ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata},
    registry::ConfigItemRegistration,
};

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
                description: "从本机配置文件读取的共享访问令牌，与中转地址一起生效。".to_owned(),
                data_type: ConfigDataType::String,
                remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                required: false,
                default: Some(Value::String(String::new())),
                dictionary: None,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
        ],
    }
}
