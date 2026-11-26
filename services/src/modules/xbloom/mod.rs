mod protocol;
mod routes;
mod service;

#[cfg(test)]
mod tests;

pub use service::XbloomService;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static XBLOOM_MODULE: ModuleRegistration = ModuleRegistration {
    id: "xbloom",
    name: "xBloom",
    description: "查询 Org 配方库和日记中的咖啡配方，并在日记中新建和修改配方。",
    version: env!("CARGO_PKG_VERSION"),
};
