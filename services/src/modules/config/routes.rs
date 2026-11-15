use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::Value;

use super::{
    model::ConfigModuleMetadata,
    protocol::{ConfigErrorResponse, MetaResponse, ModuleDataResponse, ValueResponse},
    service::{ConfigService, ConfigServiceError},
};

pub fn router(service: Arc<ConfigService>) -> Router {
    Router::new()
        .route("/meta", get(meta))
        .route("/meta/{module_id}", get(module_meta))
        .route("/data/{module_id}", get(module_data))
        .route(
            "/data/{module_id}/{key}",
            get(get_value).put(put_value).post(post_value),
        )
        .with_state(service)
}

async fn meta(State(service): State<Arc<ConfigService>>) -> Json<MetaResponse> {
    Json(MetaResponse {
        modules: service.metadata().await,
    })
}

async fn module_meta(
    Path(module_id): Path<String>,
    State(service): State<Arc<ConfigService>>,
) -> Result<Json<ConfigModuleMetadata>, Response> {
    service
        .metadata()
        .await
        .into_iter()
        .find(|metadata| metadata.id == module_id)
        .map(Json)
        .ok_or_else(|| error_response(ConfigServiceError::ModuleNotFound(module_id)))
}

async fn module_data(
    Path(module_id): Path<String>,
    State(service): State<Arc<ConfigService>>,
) -> Result<Json<ModuleDataResponse>, Response> {
    service
        .module_values(&module_id)
        .await
        .map(|values| Json(ModuleDataResponse { module_id, values }))
        .map_err(error_response)
}

async fn get_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
) -> Result<Json<ValueResponse>, Response> {
    service
        .value(&module_id, &key)
        .await
        .map(|value| {
            Json(ValueResponse {
                module_id,
                key,
                value,
            })
        })
        .map_err(error_response)
}

async fn put_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    Json(value): Json<Value>,
) -> Result<Json<ValueResponse>, Response> {
    service
        .replace(&module_id, &key, value)
        .await
        .map(|value| {
            Json(ValueResponse {
                module_id,
                key,
                value,
            })
        })
        .map_err(error_response)
}

async fn post_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    Json(value): Json<Value>,
) -> Result<Json<ValueResponse>, Response> {
    service
        .partial(&module_id, &key, value)
        .await
        .map(|value| {
            Json(ValueResponse {
                module_id,
                key,
                value,
            })
        })
        .map_err(error_response)
}

fn error_response(error: ConfigServiceError) -> Response {
    let status = match error {
        ConfigServiceError::ModuleNotFound(_) | ConfigServiceError::ItemNotFound { .. } => {
            StatusCode::NOT_FOUND
        }
        ConfigServiceError::ValueNotSet { .. } => StatusCode::NOT_FOUND,
        ConfigServiceError::InvalidValue(_) => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(ConfigErrorResponse {
            code: error_code(&error).to_owned(),
            message: error.to_string(),
        }),
    )
        .into_response()
}

fn error_code(error: &ConfigServiceError) -> &'static str {
    match error {
        ConfigServiceError::ModuleNotFound(_) => "module-not-found",
        ConfigServiceError::ItemNotFound { .. } => "config-not-found",
        ConfigServiceError::ValueNotSet { .. } => "config-not-set",
        ConfigServiceError::InvalidValue(_) => "invalid-config",
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, body::to_bytes, http::Request};
    use serde_json::{Value, json};
    use tower::util::ServiceExt;

    use super::*;
    use crate::modules::config::model::{ConfigDataType, ConfigItemMetadata};

    fn service() -> Arc<ConfigService> {
        Arc::new(ConfigService::new(ConfigModuleMetadata {
            id: "module_a".to_owned(),
            name: "模块 A".to_owned(),
            description: "测试模块".to_owned(),
            version: "0.1.0".to_owned(),
            items: vec![
                ConfigItemMetadata {
                    key: "config_a".to_owned(),
                    name: "配置 A".to_owned(),
                    description: "字符串".to_owned(),
                    data_type: ConfigDataType::String,
                    required: false,
                    default: Some(json!("默认值")),
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                },
                ConfigItemMetadata {
                    key: "config_b".to_owned(),
                    name: "配置 B".to_owned(),
                    description: "字典".to_owned(),
                    data_type: ConfigDataType::Dictionary,
                    required: false,
                    default: None,
                    dictionary: None,
                    list: None,
                    checker: None,
                    file_path: None,
                },
            ],
        }))
    }

    async fn json_request(
        service: Arc<ConfigService>,
        method: &str,
        uri: &str,
        body: Value,
    ) -> Value {
        let response = router(service)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn supports_meta_and_value_routes() {
        let response = router(service())
            .oneshot(
                Request::builder()
                    .uri("/meta/module_a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let config_service = service();
        let body = json_request(
            config_service.clone(),
            "PUT",
            "/data/module_a/config_a",
            json!("已设置"),
        )
        .await;
        assert_eq!(body["value"], "已设置");

        let body = json_request(
            config_service,
            "GET",
            "/data/module_a/config_a",
            Value::Null,
        )
        .await;
        assert_eq!(body["value"], "已设置");
    }

    #[tokio::test]
    async fn post_merges_dictionary_values() {
        let config_service = service();
        json_request(
            config_service.clone(),
            "PUT",
            "/data/module_a/config_b",
            json!({"a": 1}),
        )
        .await;
        let body = json_request(
            config_service,
            "POST",
            "/data/module_a/config_b",
            json!({"b": 2}),
        )
        .await;
        assert_eq!(body["value"], json!({"a": 1, "b": 2}));
    }
}
