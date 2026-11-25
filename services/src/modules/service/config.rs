use loki_services_macros::loki_config;
use serde_json::json;

use crate::{
    modules::config::{
        model::{
            ConfigDataType, ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata,
            ConfigRemoteAccess, IntegerRange,
        },
        registry::ConfigItemRegistration,
        service::{ConfigService, ConfigServiceError},
    },
    runtime::DEFAULT_HTTP_PORT,
};

const MODULE: &str = "service";
const HTTP_PORT: &str = "http_port";

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static HTTP_PORT_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: HTTP_PORT,
    options: || ConfigItemOptions {
        integer_range: Some(IntegerRange {
            min: 1,
            max: i64::from(u16::MAX),
        }),
        ..ConfigItemOptions::default()
    },
};

#[loki_config]
fn metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: MODULE.into(),
        name: "服务".into(),
        description: "配置本机 services 进程。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![ConfigItemMetadata {
            key: HTTP_PORT.into(),
            name: "HTTP 监听端口".into(),
            description:
                "配置固定端口（1 至 65535），保存后在下次启动时生效。端口被占用时发送系统通知。"
                    .into(),
            data_type: ConfigDataType::Integer,
            remote_access: ConfigRemoteAccess::Local,
            required: false,
            default: Some(json!(DEFAULT_HTTP_PORT)),
            dictionary: None,
            list: None,
            checker: None,
            file_path: None,
            executable: None,
        }],
    }
}

pub(crate) async fn http_port(config: &ConfigService) -> Result<u16, ConfigServiceError> {
    let value = config.value(MODULE, HTTP_PORT).await?.value;
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .filter(|&port| port != 0)
        .ok_or_else(|| ConfigServiceError::InvalidValue("HTTP 监听端口无效。".into()))
}
