use std::sync::Arc;

pub mod error;

use axum::Router;

use crate::runtime::RuntimeState;

pub fn router(runtime: Arc<RuntimeState>) -> Router {
    Router::new().merge(runtime.services().router(runtime))
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    runtime: Arc<RuntimeState>,
) -> Result<(), std::io::Error> {
    axum::serve(listener, router(runtime)).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{body::Body, body::to_bytes, http::Request};
    use serde_json::Value;
    use tower::util::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn health_route_returns_json() {
        let app = router(Arc::new(RuntimeState::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "application/json");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["service"], "loki-services");
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn unknown_route_is_not_exposed() {
        let app = router(Arc::new(RuntimeState::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }

    #[tokio::test]
    async fn services_registry_mounts_discover_module() {
        let app = router(Arc::new(RuntimeState::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/discover/modules/discover")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
    }
}
