pub mod model;
pub mod protocol;
pub mod registry;
mod routes;
mod service;
#[cfg(test)]
mod tests;
mod validation;

use crate::modules::discover::registry::ModuleRegistration;
use axum::Router;
pub use service::{StatusError, StatusService};
use std::sync::Arc;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static STATUS_MODULE: ModuleRegistration = ModuleRegistration {
    id: "status",
    name: "状态中心",
    description: "接收各模块发布的数据并提供统一状态查询。",
    version: env!("CARGO_PKG_VERSION"),
};

impl StatusService {
    pub fn router(self: &Arc<Self>) -> Router {
        routes::router(Arc::clone(self))
    }
}
