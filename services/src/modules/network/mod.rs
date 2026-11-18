mod config;
mod identity;
pub mod protocol;
mod relay;
mod service;
mod status;

pub use identity::IdentityStore;
pub use relay::{IrohRelayRegistrar, RelayRegistrar, RelaySession};
pub use service::NetworkService;

#[cfg(test)]
mod tests;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static NETWORK_MODULE: ModuleRegistration = ModuleRegistration {
    id: "network",
    name: "网络",
    description: "管理本地网络身份与中转注册。",
    version: env!("CARGO_PKG_VERSION"),
};
