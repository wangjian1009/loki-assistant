pub mod config;
mod mail;
pub mod protocol;
mod routes;
pub mod service;
mod status;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;

use crate::modules::{
    config::service::ConfigService, discover::registry::ModuleRegistration,
    notification::NotificationService, status::StatusService,
};

pub(crate) use self::routes::error_response;
use self::{config::EmacsConfig, service::EmacsService};

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static EMACS_MODULE: ModuleRegistration = ModuleRegistration {
    id: "emacs",
    name: "Emacs",
    description: "执行 Emacs Lisp、查询 buffer 并分发模块通知。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<EmacsService>,
}

impl Module {
    pub fn new(
        config: Arc<ConfigService>,
        notification: Arc<NotificationService>,
        status: Arc<StatusService>,
    ) -> Self {
        Self {
            service: Arc::new(EmacsService::new(
                EmacsConfig::new(config),
                notification,
                status,
            )),
        }
    }

    pub fn service(&self) -> Arc<EmacsService> {
        Arc::clone(&self.service)
    }

    pub fn router(&self) -> Router {
        routes::router(self.service())
    }
}
