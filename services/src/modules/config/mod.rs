pub mod model;

mod protocol;
mod routes;
pub mod service;

use std::sync::Arc;

use axum::Router;

use crate::modules::discover::model::{
    ApiMetadata, ModuleDescriptor, ModuleMetadata, ParameterMetadata,
};

use self::{
    model::{ConfigMetadataProvider, ConfigModuleMetadata},
    service::{ConfigService, ModuleSubscription},
};

#[derive(Clone, Debug)]
pub struct Module {
    metadata: ConfigModuleMetadata,
    service: Arc<ConfigService>,
}

impl Module {
    pub fn new() -> Self {
        let metadata = ConfigModuleMetadata {
            id: "config".to_owned(),
            name: "配置中心".to_owned(),
            description: "提供配置模型、配置数据读取、更新和变化订阅。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            items: Vec::new(),
        };
        let service = Arc::new(ConfigService::new(metadata.clone()));
        Self { metadata, service }
    }

    pub fn router(&self) -> Router {
        routes::router(Arc::clone(&self.service))
    }

    pub fn service(&self) -> Arc<ConfigService> {
        Arc::clone(&self.service)
    }

    pub fn subscribe_module(&self, module_id: &str) -> ModuleSubscription {
        self.service.subscribe_module(module_id)
    }
}

impl Default for Module {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigMetadataProvider for Module {
    fn config_metadata(&self) -> ConfigModuleMetadata {
        self.metadata.clone()
    }
}

impl ModuleDescriptor for Module {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "config".to_owned(),
            name: "配置中心".to_owned(),
            description: "提供配置模型、配置数据读取、更新和变化订阅。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            apis: vec![
                ApiMetadata::get(
                    "meta",
                    "/meta",
                    "获取完整配置模型",
                    "返回所有配置模块及配置项类型、默认值和检查器描述。",
                ),
                ApiMetadata::get(
                    "module-meta",
                    "/meta/{module_id}",
                    "获取模块配置模型",
                    "返回单个配置模块的完整描述。",
                )
                .with_parameters(vec![ParameterMetadata::path(
                    "module_id",
                    "string",
                    "配置模块标识",
                )]),
                ApiMetadata::get(
                    "module-data",
                    "/data/{module_id}",
                    "获取模块配置数据",
                    "只返回已经设置的配置值，不填充元数据中的默认值。",
                )
                .with_parameters(vec![ParameterMetadata::path(
                    "module_id",
                    "string",
                    "配置模块标识",
                )]),
                ApiMetadata::get(
                    "value",
                    "/data/{module_id}/{key}",
                    "获取配置项",
                    "返回单个已经设置的配置值。",
                )
                .with_parameters(config_value_parameters()),
                ApiMetadata::new(
                    "set-value",
                    "PUT",
                    "/data/{module_id}/{key}",
                    "设置完整配置值",
                    "替换一个配置项的完整值。",
                )
                .with_parameters(config_value_parameters()),
                ApiMetadata::new(
                    "patch-value",
                    "POST",
                    "/data/{module_id}/{key}",
                    "部分更新配置值",
                    "合并字典或追加列表项。",
                )
                .with_parameters(config_value_parameters()),
            ],
        }
    }
}

fn config_value_parameters() -> Vec<ParameterMetadata> {
    vec![
        ParameterMetadata::path("module_id", "string", "配置模块标识"),
        ParameterMetadata::path("key", "string", "配置项标识"),
    ]
}
