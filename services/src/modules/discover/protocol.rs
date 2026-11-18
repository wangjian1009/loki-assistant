use serde::Serialize;

use super::model::{ModuleMetadata, ModuleSummary};

#[derive(Clone, Debug, Serialize)]
pub struct DiscoverResponse {
    pub service: String,
    pub version: String,
    pub modules: Vec<ModuleMetadata>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleListResponse {
    pub modules: Vec<ModuleSummary>,
}
