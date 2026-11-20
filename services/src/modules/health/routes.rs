use std::sync::Arc;

use axum::{Json, Router, extract::State};
use loki_services_macros::loki_api;

use super::protocol::HealthResponse;
use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    crate::http::mount_routes(Router::new(), "health").with_state(runtime)
}

#[loki_api(
    module = "health",
    mount = "Arc<RuntimeState>",
    id = "healthz",
    method = "GET",
    path = "/healthz",
    access = "paired",
    summary = "读取服务健康状态",
    description = "返回服务名称、版本和运行时间。"
)]
async fn healthz(State(runtime): State<Arc<RuntimeState>>) -> Json<HealthResponse> {
    Json(HealthResponse::from_runtime(&runtime))
}
