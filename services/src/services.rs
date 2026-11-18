use std::sync::Arc;

use axum::Router;

use crate::{
    modules::{config, control_center, discover, emacs, health, notification::NotificationService},
    runtime::RuntimeState,
};

/// 控制中心服务模块的组合根。
#[derive(Debug)]
pub struct Services {
    pub health: health::Module,
    pub discover: discover::Module,
    pub config: config::Module,
    pub notification: Arc<NotificationService>,
    pub emacs: emacs::Module,
}

/// 与需求文档中的 CoreService 名称兼容的别名。
pub type CoreService = Services;

impl Services {
    pub fn build() -> Self {
        Self::with_config(config::service::ConfigService::from_registry())
    }

    pub fn load() -> Result<Self, config::service::ConfigServiceError> {
        Ok(Self::with_config(config::service::ConfigService::load(
            config::service::ConfigService::storage_path()?,
        )?))
    }

    fn with_config(config: config::service::ConfigService) -> Self {
        let config = config::Module::from_service(
            config.with_change_handler("control-center", control_center::handler()),
        );
        let health = health::Module::new();
        let discover = discover::Module::new();
        let notification = Arc::new(NotificationService::new());
        let emacs = emacs::Module::new(config.service(), Arc::clone(&notification));

        Self {
            health,
            discover,
            config,
            notification,
            emacs,
        }
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        Router::new()
            .merge(self.health.router(runtime))
            .merge(self.discover.router())
            .merge(self.config.router())
            .merge(self.notification.router())
            .merge(self.emacs.router())
    }
}
