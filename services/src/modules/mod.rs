//! 控制中心业务模块。

pub mod health;

use std::sync::Arc;

use axum::Router;

use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    Router::new().merge(health::router(runtime))
}
