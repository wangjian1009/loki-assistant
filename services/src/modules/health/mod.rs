mod protocol;
mod routes;

use std::sync::Arc;

use axum::Router;

use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    routes::router(runtime)
}
