use linkme::distributed_slice;

use super::model::{ConfigItemOptions, ConfigModuleMetadata, NamedType};
use std::collections::BTreeMap;

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

pub struct ConfigTypeRegistration {
    pub module_id: &'static str,
    pub name: &'static str,
    pub definition: fn() -> NamedType,
}

#[distributed_slice]
pub static CONFIG_TYPES: [ConfigTypeRegistration];

pub(super) fn named_type(module: &str, name: &str) -> Option<NamedType> {
    CONFIG_TYPES
        .iter()
        .find(|entry| entry.module_id == module && entry.name == name)
        .map(|entry| (entry.definition)())
}

pub(super) fn named_types(module: &str) -> BTreeMap<&'static str, NamedType> {
    CONFIG_TYPES
        .iter()
        .filter(|entry| entry.module_id == module)
        .map(|entry| (entry.name, (entry.definition)()))
        .collect()
}

pub(super) fn item_options(module_id: &str, key: &str) -> ConfigItemOptions {
    CONFIG_ITEMS
        .iter()
        .find(|entry| entry.module_id == module_id && entry.key == key)
        .map(|entry| (entry.options)())
        .unwrap_or_default()
}
