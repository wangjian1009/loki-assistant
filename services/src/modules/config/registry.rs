use linkme::distributed_slice;

use super::model::{ConfigItemOptions, ConfigModuleMetadata};

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

pub struct ConfigItemRegistration {
    pub module_id: &'static str,
    pub key: &'static str,
    pub options: fn() -> ConfigItemOptions,
}

#[distributed_slice]
pub static CONFIG_ITEMS: [ConfigItemRegistration];

pub(super) fn item_options(module_id: &str, key: &str) -> ConfigItemOptions {
    CONFIG_ITEMS
        .iter()
        .find(|entry| entry.module_id == module_id && entry.key == key)
        .map(|entry| (entry.options)())
        .unwrap_or_default()
}
