mod protocol;
mod routes;

use std::sync::Arc;

use axum::Router;

use crate::modules::discover::model::{
    ApiMetadata, DataMetadata, FieldMetadata, ModuleDescriptor, ModuleMetadata,
};
use crate::runtime::RuntimeState;

#[derive(Clone, Debug, Default)]
pub struct Module;

impl Module {
    pub fn new() -> Self {
        Self
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        routes::router(runtime)
    }
}

impl ModuleDescriptor for Module {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "health".to_owned(),
            name: "服务健康".to_owned(),
            description: "提供控制中心服务进程的健康状态。".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            apis: vec![ApiMetadata {
                id: "healthz".to_owned(),
                method: "GET".to_owned(),
                path: "/healthz".to_owned(),
                summary: "读取服务健康状态".to_owned(),
                description: "返回服务名称、版本和运行时间。".to_owned(),
                parameters: Vec::new(),
                request: None,
                response: Some(DataMetadata {
                    name: "HealthResponse".to_owned(),
                    fields: vec![
                        FieldMetadata::required("service", "string", "服务名称"),
                        FieldMetadata::required("status", "string", "健康状态"),
                        FieldMetadata::required("version", "string", "服务版本"),
                        FieldMetadata::required("uptime_seconds", "integer", "运行秒数"),
                    ],
                }),
            }],
        }
    }
}
