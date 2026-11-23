mod protocol;
mod routes;
mod service;
mod status;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;

pub use protocol::{Alert, AlertInput, AlertLevel};
pub use service::{AlertError, AlertService};

use crate::modules::discover::registry::ModuleRegistration;

const MODULE_ID: &str = "alert";
const ACTIVE_KEY: &str = "active";

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static ALERT_MODULE: ModuleRegistration = ModuleRegistration {
    id: MODULE_ID,
    name: "告警",
    description: "发布和清理告警，通过状态中心提供活跃告警。",
    version: env!("CARGO_PKG_VERSION"),
};

impl AlertService {
    pub fn router(self: &Arc<Self>) -> Router {
        routes::router(Arc::clone(self))
    }
}
