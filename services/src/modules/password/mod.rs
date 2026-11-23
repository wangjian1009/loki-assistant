pub mod protocol;
mod routes;
pub mod service;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;

use crate::modules::discover::registry::ModuleRegistration;

use self::service::{PasswordEnvironment, PasswordService};

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static PASSWORD_MODULE: ModuleRegistration = ModuleRegistration {
    id: "password",
    name: "密码管理",
    description: "查询本地密码库的条目，并通过 GPG 解密原文。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<PasswordService>,
}

impl Module {
    pub(crate) fn new(environment: Option<PasswordEnvironment>) -> Self {
        Self {
            service: Arc::new(PasswordService::new(environment)),
        }
    }

    pub fn service(&self) -> Arc<PasswordService> {
        Arc::clone(&self.service)
    }

    pub fn router(&self) -> Router {
        routes::router(self.service())
    }
}
