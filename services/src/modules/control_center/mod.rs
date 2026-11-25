mod config;
mod service;

pub use service::ControlCenterService;
pub(crate) use service::notification_router;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static CONTROL_CENTER_MODULE: ModuleRegistration = ModuleRegistration {
    id: "control-center",
    name: "控制中心",
    description: "管理菜单栏程序的登录启动。",
    version: env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
mod tests;
