use crate::{
    http::error::{ErrorSource, response as error_response},
    modules::network::remote::AuthenticatedPeer,
};
use axum::{Extension, Json, Router, extract::Path, http::StatusCode, response::Response};

use super::{
    model::ModuleMetadata,
    protocol::{DiscoverResponse, ModuleListResponse},
    registry,
};

use loki_services_macros::loki_api;

pub fn router() -> Router {
    crate::http::mount_routes(Router::new(), "discover")
}

#[loki_api(
    module = "discover",
    mount = "()",
    id = "discover",
    method = "GET",
    path = "/discover",
    access = "paired",
    summary = "获取完整服务描述",
    description = "返回当前请求方可访问的模块及其 HTTP API 元数据。"
)]
async fn discover(peer: Option<Extension<AuthenticatedPeer>>) -> Json<DiscoverResponse> {
    Json(DiscoverResponse {
        service: "loki-services".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        modules: registry::metadata_for(peer.is_some()),
    })
}

#[loki_api(
    module = "discover",
    mount = "()",
    id = "list-modules",
    method = "GET",
    path = "/discover/modules",
    access = "paired",
    summary = "获取模块列表",
    description = "返回当前请求方可访问的模块名称、版本和简要描述。"
)]
async fn list_modules(peer: Option<Extension<AuthenticatedPeer>>) -> Json<ModuleListResponse> {
    let catalog = registry::metadata_for(peer.is_some());
    Json(ModuleListResponse {
        modules: catalog.iter().map(ModuleMetadata::summary).collect(),
    })
}

#[loki_api(
    module = "discover",
    mount = "()",
    id = "get-module",
    method = "GET",
    path = "/discover/modules/{module_id}",
    access = "paired",
    summary = "获取模块描述",
    description = "根据模块标识返回当前请求方可访问的 API 元数据。"
)]
async fn get_module(
    Path(module_id): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Result<Json<ModuleMetadata>, Response> {
    registry::metadata_for(peer.is_some())
        .into_iter()
        .find(|module| module.id == module_id)
        .map(Json)
        .ok_or_else(|| {
            error_response(
                StatusCode::NOT_FOUND,
                ErrorSource::Services,
                "module-not-found",
                format!("未找到模块：{module_id}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, body::to_bytes, http::Request};
    use serde_json::Value;
    use tower::util::ServiceExt;

    use super::*;
    use crate::modules::discover::Module;

    fn app() -> Router {
        Module::new().router()
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
        let discover = body["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == "discover")
            .unwrap();
        assert!(discover["apis"].is_array());
        let config = body["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == "config")
            .unwrap();
        let get_module = config["apis"]
            .as_array()
            .unwrap()
            .iter()
            .find(|api| api["id"] == "module-meta")
            .unwrap();
        assert_eq!(get_module["parameters"][0]["name"], "module_id");
    }

    #[tokio::test]
    async fn returns_module_summaries() {
        let (status, body) = get_json("/discover/modules").await;
        assert_eq!(status, StatusCode::OK);
        let discover = body["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == "discover")
            .unwrap();
        assert!(discover.get("apis").is_none());
    }

    #[tokio::test]
    async fn returns_single_module_and_not_found_error() {
        let (status, body) = get_json("/discover/modules/discover").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "discover");

        let (status, body) = get_json("/discover/modules/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "module-not-found");
    }
}
