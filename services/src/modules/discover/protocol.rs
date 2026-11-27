use serde::Serialize;

use super::model::{ModuleMetadata, ModuleSummary};

#[derive(Clone, Debug, Serialize)]
pub struct DiscoverResponse {
    /// 服务标识，例如 `loki-services`。
    pub service: String,
    /// 当前运行的服务版本。
    pub version: String,
    /// 当前请求方可访问的模块及其 API 元数据。
    pub modules: Vec<ModuleMetadata>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleListResponse {
    /// 当前请求方可访问的模块摘要。
    pub modules: Vec<ModuleSummary>,
}
