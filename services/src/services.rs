use std::sync::Arc;

use axum::Router;

use crate::modules::discover::model::ModuleDescriptor;
use crate::{
    modules::{config, discover, health},
    runtime::RuntimeState,
};

/// 控制中心服务模块的组合根。
#[derive(Debug)]
pub struct Services {
    pub health: health::Module,
    pub discover: discover::Module,
    pub config: config::Module,
}

/// 与需求文档中的 CoreService 名称兼容的别名。
pub type CoreService = Services;

impl Services {
    pub fn build() -> Self {
        let health = health::Module::new();
        let discover = discover::Module::new();
        let config = config::Module::new();
        let catalog = vec![health.metadata(), discover.metadata(), config.metadata()];

        Self {
            health,
            discover: discover.with_catalog(catalog),
            config,
        }
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        Router::new()
            .merge(self.health.router(runtime))
            .merge(self.discover.router())
            .merge(self.config.router())
    }
}
