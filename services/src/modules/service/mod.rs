mod config;
mod service;
mod status;

use std::sync::Arc;

use crate::modules::{discover::registry::ModuleRegistration, status::StatusService};

pub(crate) use config::http_port;
pub use service::SelfService;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static SERVICE_MODULE: ModuleRegistration = ModuleRegistration {
    id: "service",
    name: "服务",
    description: "发布本机 services 进程的自身运行信息。",
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug)]
pub struct Module {
    service: Arc<SelfService>,
}

impl Module {
    pub fn new(status: Arc<StatusService>) -> Self {
        Self {
            service: Arc::new(SelfService::new(status)),
        }
    }

    pub fn service(&self) -> Arc<SelfService> {
        Arc::clone(&self.service)
    }
}
