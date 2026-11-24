mod protocol;
mod routes;
mod service;

#[cfg(test)]
mod tests;

pub use service::DocService;

use crate::modules::discover::registry::ModuleRegistration;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static DOC_MODULE: ModuleRegistration = ModuleRegistration {
    id: "doc",
    name: "文档",
    description: "通过 Emacs 和 org-roam 按 ID 读取文档，按目标提交网页全文。",
    version: env!("CARGO_PKG_VERSION"),
};
