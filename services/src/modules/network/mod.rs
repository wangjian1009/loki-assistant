mod config;
mod identity;
mod pairing;
mod peers;
pub mod protocol;
mod qr_code;
mod relay;
pub mod remote;
mod routes;
mod service;
mod status;

pub use identity::IdentityStore;
pub use relay::{IrohRelayRegistrar, RelayRegistrar, RelaySession};
pub use service::NetworkService;

#[cfg(test)]
mod pairing_tests;
#[cfg(test)]
mod tests;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static NETWORK_MODULE: ModuleRegistration = ModuleRegistration {
    id: "network",
    name: "网络",
    description: "管理本地网络身份、中转注册和扫码设备绑定。",
    version: env!("CARGO_PKG_VERSION"),
};
