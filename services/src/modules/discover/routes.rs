use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};

use super::{
    model::ModuleMetadata,
    protocol::{DiscoverError, DiscoverResponse, ModuleListResponse},
};

type Catalog = Arc<Vec<ModuleMetadata>>;

pub fn router(catalog: Catalog) -> Router {
    Router::new()
        .route("/discover", get(discover))
        .route("/discover/modules", get(list_modules))
        .route("/discover/modules/{module_id}", get(get_module))
        .with_state(catalog)
}

async fn discover(State(catalog): State<Catalog>) -> Json<DiscoverResponse> {
    Json(DiscoverResponse {
        service: "loki-services".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        modules: catalog.as_ref().clone(),
    })
}

async fn list_modules(State(catalog): State<Catalog>) -> Json<ModuleListResponse> {
    Json(ModuleListResponse {
        modules: catalog.iter().map(ModuleMetadata::summary).collect(),
    })
}

async fn get_module(
    Path(module_id): Path<String>,
    State(catalog): State<Catalog>,
) -> Result<Json<ModuleMetadata>, Response> {
    catalog
        .iter()
        .find(|module| module.id == module_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(DiscoverError {
                    code: "module-not-found".to_owned(),
                    message: format!("未找到模块：{module_id}"),
                }),
            )
                .into_response()
        })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, body::to_bytes, http::Request};
    use serde_json::Value;
    use tower::util::ServiceExt;

    use super::*;
    use crate::modules::discover::{Module, model::ModuleDescriptor};

    fn app() -> Router {
        let discover = Module::new();
        router(Arc::new(vec![discover.metadata()]))
    }

    async fn get_json(path: &str) -> (StatusCode, Value) {
        let response = app()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn returns_full_module_catalog() {
        let (status, body) = get_json("/discover").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["modules"][0]["id"], "discover");
        assert!(body["modules"][0]["apis"].is_array());
    }

    #[tokio::test]
    async fn returns_module_summaries() {
        let (status, body) = get_json("/discover/modules").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["modules"][0]["id"], "discover");
        assert!(body["modules"][0].get("apis").is_none());
    }

    #[tokio::test]
    async fn returns_single_module_and_not_found_error() {
        let (status, body) = get_json("/discover/modules/discover").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "discover");

        let (status, body) = get_json("/discover/modules/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "module-not-found");
    }
}
