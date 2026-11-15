use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use loki_services_macros::loki_api;

use super::protocol::HealthResponse;
use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(runtime)
}

#[loki_api(
    module = "health",
    id = "healthz",
    method = "GET",
    path = "/healthz",
    summary = "读取服务健康状态",
    description = "返回服务名称、版本和运行时间。"
)]
async fn healthz(State(runtime): State<Arc<RuntimeState>>) -> Json<HealthResponse> {
    Json(HealthResponse::from_runtime(&runtime))
}
