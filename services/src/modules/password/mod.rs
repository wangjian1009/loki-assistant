pub mod config;
pub mod protocol;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;

use crate::modules::{config::service::ConfigService, discover::registry::ModuleRegistration};

use self::{config::PasswordConfig, service::PasswordService};

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static PASSWORD_MODULE: ModuleRegistration = ModuleRegistration {
    id: "password",
    name: "密码管理",
    description: "通过 pass 查询本地密码库的条目和解密原文，仅提供读取能力。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<PasswordService>,
}

impl Module {
    pub fn new(config: Arc<ConfigService>) -> Self {
        Self {
            service: Arc::new(PasswordService::new(PasswordConfig::new(config))),
        }
    }

    pub fn service(&self) -> Arc<PasswordService> {
        Arc::clone(&self.service)
    }

    pub fn router(&self) -> Router {
        routes::router(self.service())
    }
}
