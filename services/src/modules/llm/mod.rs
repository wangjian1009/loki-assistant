mod config;
mod monitor;
mod protocol;
mod routes;
mod service;
mod usage;

#[cfg(test)]
mod tests;

use crate::modules::discover::registry::ModuleRegistration;

pub(crate) use config::notification_router;
pub(crate) use monitor::Monitor;
pub(crate) use service::LlmService;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static LLM_MODULE: ModuleRegistration = ModuleRegistration {
    id: "llm",
    name: "AI 统一代理",
    description: "按客户端令牌转发 Codex 和 Claude 请求，记录请求状态及用量。",
    version: env!("CARGO_PKG_VERSION"),
};
