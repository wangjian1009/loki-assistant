use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};

use super::protocol::HealthResponse;
use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(runtime)
}

async fn healthz(State(runtime): State<Arc<RuntimeState>>) -> Json<HealthResponse> {
    Json(HealthResponse::from_runtime(&runtime))
}
