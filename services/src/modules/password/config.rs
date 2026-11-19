use std::sync::Arc;

use loki_services_macros::loki_config;

use crate::modules::config::{
    model::{
        CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata, ConfigValue,
        ExecutableMetadata, FilePathMetadata,
    },
    service::{ConfigService, ConfigServiceError},
};

#[loki_config]
fn password_config_metadata() -> ConfigModuleMetadata {
    PasswordConfig::metadata()
}

/// 每次查询读取配置，不缓存密码库位置或程序路径。
#[derive(Clone, Debug)]
pub struct PasswordConfig {
    service: Arc<ConfigService>,
}

impl PasswordConfig {
    pub fn new(service: Arc<ConfigService>) -> Self {
        Self { service }
    }

    pub async fn store_dir(&self) -> Result<ConfigValue, ConfigServiceError> {
        self.service.value("password", "store_dir").await
    }

    pub async fn pass_path(&self) -> Result<ConfigValue, ConfigServiceError> {
        self.service.value("password", "pass_path").await
    }

    pub fn metadata() -> ConfigModuleMetadata {
        ConfigModuleMetadata {
            id: "password".to_owned(),
            name: "密码管理".to_owned(),
            description: "配置只读 pass 密码库；修改后在下次查询时生效。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            items: vec![
                ConfigItemMetadata {
                    key: "store_dir".to_owned(),
                    name: "本地 pass 目录".to_owned(),
                    description: "密码库的绝对目录；必须显式设置，不展开 ~ 和环境变量。".to_owned(),
                    data_type: ConfigDataType::FilePath,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: Some(CheckerMetadata {
                        pattern: r"^/[^\x00\r\n]*$".to_owned(),
                        description: "请使用不含 NUL 和换行的绝对目录。".to_owned(),
                    }),
                    file_path: Some(FilePathMetadata {
                        default: None,
                        must_exist: true,
                        directory: true,
                    }),
                    executable: None,
                },
                ConfigItemMetadata {
                    key: "pass_path".to_owned(),
                    name: "pass 位置".to_owned(),
                    description: "pass 可执行文件或 PATH 中的命令名；不展开 ~ 和环境变量。"
                        .to_owned(),
                    data_type: ConfigDataType::Executable,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: Some(FilePathMetadata {
                        default: None,
                        must_exist: true,
                        directory: false,
                    }),
                    executable: Some(ExecutableMetadata {
                        default_program: "pass".to_owned(),
                        search_paths: vec![
                            "/opt/homebrew/bin".to_owned(),
                            "/usr/local/bin".to_owned(),
                        ],
                    }),
                },
            ],
        }
    }
}
