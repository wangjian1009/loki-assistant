use std::sync::Arc;

use axum::Router;

use crate::{
    modules::{config, discover, emacs, health},
    runtime::RuntimeState,
};

/// 控制中心服务模块的组合根。
#[derive(Debug)]
pub struct Services {
    pub health: health::Module,
    pub discover: discover::Module,
    pub config: config::Module,
    pub emacs: emacs::Module,
}

/// 与需求文档中的 CoreService 名称兼容的别名。
pub type CoreService = Services;

impl Services {
    pub fn build() -> Self {
        Self::with_config(config::Module::new())
    }

    pub fn load() -> Result<Self, config::service::ConfigServiceError> {
        Ok(Self::with_config(config::Module::load()?))
    }

    fn with_config(config: config::Module) -> Self {
        let health = health::Module::new();
        let discover = discover::Module::new();
        let emacs = emacs::Module::new(config.service());

        Self {
            health,
            discover,
            config,
            emacs,
        }
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        Router::new()
            .merge(self.health.router(runtime))
            .merge(self.discover.router())
            .merge(self.config.router())
            .merge(self.emacs.router())
    }
}
