mod protocol;
mod routes;

use std::sync::Arc;

use axum::Router;

use crate::modules::discover::registry::ModuleRegistration;
use crate::runtime::RuntimeState;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static HEALTH_MODULE: ModuleRegistration = ModuleRegistration {
    id: "health",
    name: "服务健康",
    description: "提供控制中心服务进程的健康状态。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug, Default)]
pub struct Module;

impl Module {
    pub fn new() -> Self {
        Self
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        routes::router(runtime)
    }
}
