pub mod model;
pub mod registry;

pub(crate) mod executable;
mod protocol;
mod routes;
pub mod service;
mod storage;

use std::sync::Arc;

use axum::Router;
use loki_services_macros::loki_config;

use crate::modules::discover::registry::ModuleRegistration;

use self::{
    model::{ConfigMetadataProvider, ConfigModuleMetadata},
    service::{ConfigService, ModuleSubscription},
};

#[loki_config]
fn config_metadata() -> ConfigModuleMetadata {
    Module::config_metadata()
}

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static CONFIG_MODULE: ModuleRegistration = ModuleRegistration {
    id: "config",
    name: "配置中心",
    description: "提供配置模型、配置数据读取、更新和变化订阅。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<ConfigService>,
}

impl Module {
    pub fn new() -> Self {
        Self {
            service: Arc::new(ConfigService::from_registry()),
        }
    }

    pub fn load() -> Result<Self, service::ConfigServiceError> {
        Ok(Self {
            service: Arc::new(ConfigService::load(ConfigService::storage_path()?)?),
        })
    }

    pub fn from_service(service: ConfigService) -> Self {
        Self {
            service: Arc::new(service),
        }
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
    fn config_metadata() -> ConfigModuleMetadata {
        Self::config_metadata()
    }
}

impl Module {
    pub fn config_metadata() -> ConfigModuleMetadata {
        ConfigModuleMetadata {
            id: "config".to_owned(),
            name: "配置中心".to_owned(),
            description: "提供配置模型、配置数据读取、更新和变化订阅。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            items: Vec::new(),
        }
    }
}
