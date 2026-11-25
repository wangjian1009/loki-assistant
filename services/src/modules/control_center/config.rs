use crate::modules::config::registry::ConfigItemRegistration;
use loki_services_macros::loki_config;
use serde_json::Value;

pub(super) const NOTIFICATION: &str = "/control-center/config/center-auto-start";

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static AUTO_START: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "control-center",
    key: "center_auto_start",
    options: || ConfigItemOptions {
        notification_endpoint: Some(NOTIFICATION),
        ..ConfigItemOptions::default()
    },
};

use crate::modules::config::model::{
    ConfigDataType, ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata,
};

#[loki_config]
fn control_center_metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "control-center".to_owned(),
        name: "控制中心".to_owned(),
        description: "控制菜单栏程序的启动行为。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![ConfigItemMetadata {
            key: "center_auto_start".to_owned(),
            name: "登录时自动启动".to_owned(),
            description: "登录 macOS 后自动启动菜单栏程序；系统操作成功后才保存。".to_owned(),
            data_type: ConfigDataType::Boolean,
            remote_access: crate::modules::config::model::ConfigRemoteAccess::Read,
            required: false,
            default: Some(Value::Bool(false)),
            dictionary: None,
            list: None,
            checker: None,
            file_path: None,
            executable: None,
        }],
    }
}
