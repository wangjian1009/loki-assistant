use super::model::StatusModuleMetadata;
use linkme::distributed_slice;

#[derive(Clone, Copy)]
pub struct StatusModuleRegistration {
    pub metadata: fn() -> StatusModuleMetadata,
}

#[distributed_slice]
pub static STATUS_MODULES: [StatusModuleRegistration];

pub fn metadata() -> Vec<StatusModuleMetadata> {
    let mut modules: Vec<_> = STATUS_MODULES
        .iter()
        .map(|entry| (entry.metadata)())
        .collect();
    modules.sort_by(|left, right| left.id.cmp(&right.id));
    modules
}
