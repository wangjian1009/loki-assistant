mod config;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static NETWORK_MODULE: ModuleRegistration = ModuleRegistration {
    id: "network",
    name: "网络",
    description: "管理中转服务器地址配置。",
    version: env!("CARGO_PKG_VERSION"),
};
