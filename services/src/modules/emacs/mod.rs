pub mod protocol;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use serde_json::json;

use crate::modules::{
    config::{
        model::{CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata},
        registry::ConfigModuleRegistration,
        service::ConfigService,
    },
    discover::registry::ModuleRegistration,
};

use self::service::EmacsService;

pub const DEFAULT_EMACSCLIENT: &str = "emacsclient";

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static EMACS_MODULE: ModuleRegistration = ModuleRegistration {
    id: "emacs",
    name: "Emacs",
    description: "执行 Emacs Lisp、查询 buffer 并分发模块通知。",
    version: env!("CARGO_PKG_VERSION"),
};

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_MODULES)]
static EMACS_CONFIG: ConfigModuleRegistration = ConfigModuleRegistration {
    metadata: Module::config_metadata,
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<EmacsService>,
}

impl Module {
    pub fn new(config: Arc<ConfigService>) -> Self {
        Self {
            service: Arc::new(EmacsService::new(config)),
        }
    }

    pub fn service(&self) -> Arc<EmacsService> {
        Arc::clone(&self.service)
    }

    pub fn router(&self) -> Router {
        routes::router(self.service())
    }

    pub fn config_metadata() -> ConfigModuleMetadata {
        ConfigModuleMetadata {
            id: "emacs".to_owned(),
            name: "Emacs".to_owned(),
            description: "配置 Emacs 连接；修改后在下次调用时生效。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            items: vec![ConfigItemMetadata {
                key: "emacsclient_path".to_owned(),
                name: "emacsclient 位置".to_owned(),
                description: "可执行文件路径或 PATH 中的命令名；不展开 ~ 和环境变量。".to_owned(),
                data_type: ConfigDataType::FilePath,
                required: false,
                default: Some(json!(DEFAULT_EMACSCLIENT)),
                dictionary: None,
                list: None,
                checker: Some(CheckerMetadata {
                    pattern: r"^[^\x00]*[^\x00\s][^\x00]*$".to_owned(),
                    description: "路径不能为空或包含 NUL。".to_owned(),
                }),
                file_path: Some(crate::modules::config::model::FilePathMetadata {
                    default: Some(DEFAULT_EMACSCLIENT.to_owned()),
                    must_exist: true,
                    directory: false,
                }),
            }],
        }
    }
}
