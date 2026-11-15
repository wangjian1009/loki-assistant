pub mod model;

mod protocol;
mod routes;

use std::sync::Arc;

use axum::Router;

use self::model::{
    ApiMetadata, DataMetadata, FieldMetadata, ModuleDescriptor, ModuleMetadata, ParameterMetadata,
};

#[derive(Clone, Debug)]
pub struct Module {
    catalog: Arc<Vec<ModuleMetadata>>,
}

impl Module {
    pub fn new() -> Self {
        Self {
            catalog: Arc::new(Vec::new()),
        }
    }

    pub fn with_catalog(self, catalog: Vec<ModuleMetadata>) -> Self {
        Self {
            catalog: Arc::new(catalog),
        }
    }

    pub fn router(&self) -> Router {
        routes::router(Arc::clone(&self.catalog))
    }
}

impl Default for Module {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleDescriptor for Module {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "discover".to_owned(),
            name: "服务发现".to_owned(),
            description: "描述控制中心提供的模块、功能和 HTTP API。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            apis: vec![
                ApiMetadata::get(
                    "discover",
                    "/discover",
                    "获取完整服务描述",
                    "返回所有模块及其 HTTP API 的完整元数据。",
                )
                .with_response(DataMetadata {
                    name: "DiscoverResponse".to_owned(),
                    fields: vec![
                        FieldMetadata::required("service", "string", "服务名称"),
                        FieldMetadata::required("version", "string", "服务版本"),
                        FieldMetadata::required("modules", "array<ModuleMetadata>", "模块列表"),
                    ],
                }),
                ApiMetadata::get(
                    "list-modules",
                    "/discover/modules",
                    "获取模块列表",
                    "返回模块名称、版本和简要描述。",
                )
                .with_response(DataMetadata {
                    name: "ModuleListResponse".to_owned(),
                    fields: vec![FieldMetadata::required(
                        "modules",
                        "array<ModuleSummary>",
                        "模块摘要列表",
                    )],
                }),
                ApiMetadata::get(
                    "get-module",
                    "/discover/modules/{module_id}",
                    "获取模块描述",
                    "根据模块标识返回单个模块的完整元数据。",
                )
                .with_parameters(vec![ParameterMetadata::path(
                    "module_id",
                    "string",
                    "模块标识",
                )])
                .with_response(DataMetadata {
                    name: "ModuleMetadata".to_owned(),
                    fields: vec![
                        FieldMetadata::required("id", "string", "模块标识"),
                        FieldMetadata::required("name", "string", "模块名称"),
                        FieldMetadata::required("description", "string", "模块描述"),
                        FieldMetadata::required("version", "string", "模块版本"),
                        FieldMetadata::required("apis", "array<ApiMetadata>", "HTTP API 列表"),
                    ],
                }),
            ],
        }
    }
}
