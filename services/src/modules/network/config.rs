use loki_services_macros::loki_config;
use serde_json::Value;

use crate::modules::config::model::{ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata};

#[loki_config]
fn network_config_metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "network".to_owned(),
        name: "网络".to_owned(),
        description: "配置中转服务器地址。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![ConfigItemMetadata {
            key: "relay_url".to_owned(),
            name: "中转地址".to_owned(),
            description:
                "中转服务器的完整 URL，例如 https://relay.example.com:8443；留空表示未配置。"
                    .to_owned(),
            data_type: ConfigDataType::String,
            required: false,
            default: Some(Value::String(String::new())),
            dictionary: None,
            list: None,
            checker: None,
            file_path: None,
            executable: None,
        }],
    }
}
