pub mod model;
pub mod registry;

mod protocol;
mod routes;

use axum::Router;

use self::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static DISCOVER_MODULE: ModuleRegistration = ModuleRegistration {
    id: "discover",
    name: "服务发现",
    description: "描述控制中心提供的模块、功能和 HTTP API。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug, Default)]
pub struct Module;

impl Module {
    pub fn new() -> Self {
        Self
    }

    pub fn router(&self) -> Router {
        routes::router()
    }
}
