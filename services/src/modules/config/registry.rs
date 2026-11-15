use linkme::distributed_slice;

use super::model::ConfigModuleMetadata;

#[derive(Clone, Copy)]
pub struct ConfigModuleRegistration {
    pub metadata: fn() -> ConfigModuleMetadata,
}

#[distributed_slice]
pub static CONFIG_MODULES: [ConfigModuleRegistration];

pub fn metadata() -> Vec<ConfigModuleMetadata> {
    CONFIG_MODULES
        .iter()
        .map(|entry| (entry.metadata)())
        .collect()
}
