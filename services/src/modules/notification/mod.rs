pub mod protocol;
pub(crate) mod routes;
mod service;
mod tracker;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;

pub use self::service::{NotificationError, NotificationService};
use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static NOTIFICATION_MODULE: ModuleRegistration = ModuleRegistration {
    id: "notification",
    name: "系统通知",
    description: "发送系统通知、封装应用内跳转并接收提交回执。",
    version: env!("CARGO_PKG_VERSION"),
};

impl NotificationService {
    pub fn router(self: &Arc<Self>) -> Router {
        routes::router(Arc::clone(self))
    }
}
