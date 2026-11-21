use std::sync::Arc;

use loki_services_macros::loki_config;
use serde_json::Value;

use crate::modules::config::{
    model::{
        CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata, ConfigValue,
    },
    service::{ConfigService, ConfigServiceError},
};

const MODULE_ID: &str = "emacs";
const EMACSCLIENT_PATH: &str = "emacsclient_path";
const SOCKET_NAME: &str = "socket_name";
const DEFAULT_EMACSCLIENT: &str = "emacsclient";
const EMACSCLIENT_SEARCH_PATHS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/Applications/Emacs.app/Contents/MacOS/bin",
    "/Applications/Emacs.app/Contents/MacOS",
];

#[loki_config]
fn emacs_config_metadata() -> ConfigModuleMetadata {
    EmacsConfig::metadata()
}

/// Emacs 模块的配置入口；各配置项在使用时读取生效值及来源。
#[derive(Clone, Debug)]
pub struct EmacsConfig {
    config_service: Arc<ConfigService>,
}

impl EmacsConfig {
    pub fn new(config_service: Arc<ConfigService>) -> Self {
        Self { config_service }
    }

    pub async fn emacsclient_path(&self) -> Result<ConfigValue, ConfigServiceError> {
        self.config_service.value(MODULE_ID, EMACSCLIENT_PATH).await
    }

    pub async fn socket_name(&self) -> Result<ConfigValue, ConfigServiceError> {
        self.config_service.value(MODULE_ID, SOCKET_NAME).await
    }

    pub fn metadata() -> ConfigModuleMetadata {
        ConfigModuleMetadata {
            id: MODULE_ID.to_owned(),
            name: "Emacs".to_owned(),
            description: "配置 Emacs 连接；修改后在下次调用时生效。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            items: vec![
                ConfigItemMetadata {
                    key: EMACSCLIENT_PATH.to_owned(),
                    name: "emacsclient 位置".to_owned(),
                    description:
                        "请选择 emacsclient 可执行文件，也可输入 PATH 中的命令名；不展开 ~ 和环境变量。"
                            .to_owned(),
                    data_type: ConfigDataType::Executable,
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: Some(CheckerMetadata {
                        pattern: r"^[^\x00]*[^\x00\s][^\x00]*$".to_owned(),
                        description: "路径不能为空或包含 NUL。".to_owned(),
                        emacs_pattern: Some("\\`[^\0]*[^\0[:space:]][^\0]*\\'".to_owned()),
                    }),
                    file_path: Some(crate::modules::config::model::FilePathMetadata {
                        default: None,
                        must_exist: true,
                        directory: false,
                    }),
                    executable: Some(crate::modules::config::model::ExecutableMetadata {
                        default_program: DEFAULT_EMACSCLIENT.to_owned(),
                        search_paths: EMACSCLIENT_SEARCH_PATHS
                            .iter()
                            .map(|path| (*path).to_owned())
                            .collect(),
                    }),
                },
                ConfigItemMetadata {
                    key: SOCKET_NAME.to_owned(),
                    name: "Emacs server socket".to_owned(),
                    description: "指定目标 Emacs 的本机 socket 名称或绝对路径；默认连接 server，不回退到其他实例。".to_owned(),
                    data_type: ConfigDataType::String,
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                    required: true,
                    default: Some(Value::String("server".to_owned())),
                    dictionary: None,
                    list: None,
                    checker: Some(CheckerMetadata {
                        pattern: r"^[^\x00]*[^\x00\s][^\x00]*$".to_owned(),
                        description: "socket 名称不能为空或包含 NUL。".to_owned(),
                        emacs_pattern: Some("\\`[^\0]*[^\0[:space:]][^\0]*\\'".to_owned()),
                    }),
                    file_path: None,
                    executable: None,
                },
            ],
        }
    }
}
