mod payload;
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
    description: "查询本地密码库条目，通过 GnuPG 获取会话密钥并在后台解密文件。",
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
