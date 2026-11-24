use linkme::distributed_slice;

use super::model::{ApiMetadata, ModuleMetadata};

#[derive(Clone, Copy, Debug)]
pub struct ModuleRegistration {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub version: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct ApiDescriptor {
    pub access: &'static str,
    pub module_id: &'static str,
    pub id: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub parameters: &'static [ParameterDescriptor],
}

#[derive(Clone, Copy, Debug)]
pub struct ParameterDescriptor {
    pub name: &'static str,
    pub location: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

#[distributed_slice]
pub static MODULE_DESCRIPTORS: [ModuleRegistration];

#[distributed_slice]
pub static API_DESCRIPTORS: [ApiDescriptor];

pub fn metadata() -> Vec<ModuleMetadata> {
    metadata_for(false)
}

pub(super) fn metadata_for(remote: bool) -> Vec<ModuleMetadata> {
    let mut modules: Vec<_> = MODULE_DESCRIPTORS
        .iter()
        .map(|module| ModuleMetadata {
            id: module.id.to_owned(),
            name: module.name.to_owned(),
            description: module.description.to_owned(),
            version: module.version.to_owned(),
            apis: Vec::new(),
        })
        .collect();

    for api in API_DESCRIPTORS
        .iter()
        .filter(|api| !remote || api.access == "paired")
    {
        if let Some(module) = modules.iter_mut().find(|module| module.id == api.module_id) {
            module.apis.push(
                ApiMetadata::new(api.id, api.method, api.path, api.summary, api.description)
                    .with_access(api.access)
                    .with_parameters(
                        api.parameters
                            .iter()
                            .map(|parameter| super::model::ParameterMetadata {
                                name: parameter.name.to_owned(),
                                location: parameter.location.to_owned(),
                                type_name: parameter.type_name.to_owned(),
                                required: parameter.required,
                                description: parameter.description.to_owned(),
                            })
                            .collect(),
                    ),
            );
        }
    }
    if remote {
        modules.retain(|module| !module.apis.is_empty());
    }
    for module in &mut modules {
        module.apis.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.method.cmp(&right.method))
        });
    }
    modules.sort_by(|left, right| left.id.cmp(&right.id));
    modules
}
