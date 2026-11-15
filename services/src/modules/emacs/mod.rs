pub mod protocol;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use loki_services_macros::loki_config;

use crate::modules::{
    config::{
        model::{CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata},
        service::ConfigService,
    },
    discover::registry::ModuleRegistration,
};

use self::service::EmacsService;

pub const DEFAULT_EMACSCLIENT: &str = "emacsclient";
const EMACSCLIENT_SEARCH_PATHS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/Applications/Emacs.app/Contents/MacOS/bin",
    "/Applications/Emacs.app/Contents/MacOS",
];

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static EMACS_MODULE: ModuleRegistration = ModuleRegistration {
    id: "emacs",
    name: "Emacs",
    description: "执行 Emacs Lisp、查询 buffer 并分发模块通知。",
    version: env!("CARGO_PKG_VERSION"),
};

#[loki_config]
fn emacs_config_metadata() -> ConfigModuleMetadata {
    Module::config_metadata()
}

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
                description:
                    "请选择 emacsclient 可执行文件，也可输入 PATH 中的命令名；不展开 ~ 和环境变量。"
                        .to_owned(),
                data_type: ConfigDataType::Executable,
                required: false,
                default: None,
                dictionary: None,
                list: None,
                checker: Some(CheckerMetadata {
                    pattern: r"^[^\x00]*[^\x00\s][^\x00]*$".to_owned(),
                    description: "路径不能为空或包含 NUL。".to_owned(),
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
            }],
        }
    }
}
