use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use loki_services_macros::loki_api;
use serde_json::Value;

use super::{
    model::{ConfigModuleMetadata, ConfigValue, ConfigValueSource},
    protocol::{
        ConfigErrorResponse, MetaResponse, ModuleDataResponse, ValidateRequest, ValidateResponse,
        ValueResponse,
    },
    service::{ConfigService, ConfigServiceError},
};

pub fn router(service: Arc<ConfigService>) -> Router {
    Router::new()
        .route("/meta", get(meta))
        .route("/config/validate", post(validate))
        .route("/meta/{module_id}", get(module_meta))
        .route("/data/{module_id}", get(module_data))
        .route(
            "/data/{module_id}/{key}",
            get(get_value).put(put_value).post(post_value),
        )
        .with_state(service)
}

#[loki_api(
    module = "config",
    id = "validate",
    method = "POST",
    path = "/config/validate",
    summary = "校验配置值",
    description = "校验 module_id、key 对应的 value，不修改配置；成功返回规范化 value，失败返回 valid=false 和错误说明。保存接口执行相同校验。"
)]
async fn validate(
    State(service): State<Arc<ConfigService>>,
    Json(request): Json<ValidateRequest>,
) -> Result<Json<ValidateResponse>, Response> {
    let (value, error) = match service
        .validate_value(&request.module_id, &request.key, &request.value)
        .await
    {
        Ok(value) => (Some(value), None),
        Err(error @ ConfigServiceError::InvalidValue(_)) => (
            None,
            Some(ConfigErrorResponse {
                code: error_code(&error).to_owned(),
                message: error.to_string(),
            }),
        ),
        Err(error) => return Err(error_response(error)),
    };
    Ok(Json(ValidateResponse {
        module_id: request.module_id,
        key: request.key,
        valid: error.is_none(),
        value,
        error,
    }))
}

#[loki_api(
    module = "config",
    id = "meta",
    method = "GET",
    path = "/meta",
    summary = "获取完整配置模型",
    description = "返回所有配置模块及配置项类型、默认值和检查器描述。"
)]
async fn meta(State(service): State<Arc<ConfigService>>) -> Json<MetaResponse> {
    Json(MetaResponse {
        modules: service.metadata().await,
    })
}

#[loki_api(
    module = "config",
    id = "module-meta",
    method = "GET",
    path = "/meta/{module_id}",
    summary = "获取模块配置模型",
    description = "返回单个配置模块的完整描述。"
)]
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

#[loki_api(
    module = "config",
    id = "module-data",
    method = "GET",
    path = "/data/{module_id}",
    summary = "获取模块配置数据",
    description = "返回模块生效配置；每项包含 value 和 source，source 为 user 或 default。"
)]
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

#[loki_api(
    module = "config",
    id = "value",
    method = "GET",
    path = "/data/{module_id}/{key}",
    summary = "获取配置项",
    description = "返回单个生效配置的 value 和 source；用户设置优先，否则返回默认值。"
)]
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
                data: value,
            })
        })
        .map_err(error_response)
}

#[loki_api(
    module = "config",
    id = "set-value",
    method = "PUT",
    path = "/data/{module_id}/{key}",
    summary = "设置完整配置值",
    description = "替换一个配置项的完整值。"
)]
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
                data: ConfigValue {
                    value,
                    source: ConfigValueSource::User,
                },
            })
        })
        .map_err(error_response)
}

#[loki_api(
    module = "config",
    id = "patch-value",
    method = "POST",
    path = "/data/{module_id}/{key}",
    summary = "部分更新配置值",
    description = "合并字典或追加列表项。"
)]
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
                data: ConfigValue {
                    value,
                    source: ConfigValueSource::User,
                },
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
        ConfigServiceError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
        ConfigServiceError::Storage(_) => "config-storage-error",
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
                    executable: None,
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
                    executable: None,
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
        assert_eq!(body["source"], "user");

        let body = json_request(
            config_service,
            "GET",
            "/data/module_a/config_a",
            Value::Null,
        )
        .await;
        assert_eq!(body["value"], "已设置");
        assert_eq!(body["source"], "user");
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
        assert_eq!(body["source"], "user");
    }

    #[tokio::test]
    async fn read_routes_return_effective_values_with_sources() {
        let service = service();
        let body = json_request(service.clone(), "GET", "/data/module_a", Value::Null).await;
        assert_eq!(
            body["values"],
            json!({"config_a":{"value":"默认值","source":"default"}})
        );
        let body = json_request(
            service.clone(),
            "GET",
            "/data/module_a/config_a",
            Value::Null,
        )
        .await;
        assert_eq!(body["value"], "默认值");
        assert_eq!(body["source"], "default");
        json_request(
            service.clone(),
            "PUT",
            "/data/module_a/config_a",
            json!("用户值"),
        )
        .await;
        let body = json_request(service, "GET", "/data/module_a", Value::Null).await;
        assert_eq!(
            body["values"]["config_a"],
            json!({"value":"用户值","source":"user"})
        );
    }

    #[tokio::test]
    async fn validate_route_is_read_only_and_invalid_writes_preserve_the_existing_value() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("loki-config-http-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("emacsclient");
        fs::write(&executable, "测试文件，不执行").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("config.yml");
        let service = Arc::new(
            ConfigService::load_with_metadata(
                &path,
                vec![crate::modules::emacs::Module::config_metadata()],
            )
            .unwrap()
            .with_executable_search_path(vec![directory.clone()]),
        );
        let mut changes = service.subscribe();
        let body = json_request(
            service.clone(),
            "POST",
            "/config/validate",
            json!({"module_id":"emacs","key":"emacsclient_path","value":"emacsclient"}),
        )
        .await;
        assert_eq!(body["valid"], true);
        assert_eq!(body["value"], json!(executable));
        assert!(!path.exists());
        assert!(changes.try_recv().is_err());
        let body = json_request(
            service.clone(),
            "POST",
            "/config/validate",
            json!({"module_id":"emacs","key":"emacsclient_path","value":42}),
        )
        .await;
        assert_eq!(body["valid"], false);
        assert_eq!(body["error"]["code"], "invalid-config");

        json_request(
            service.clone(),
            "PUT",
            "/data/emacs/emacsclient_path",
            json!(executable),
        )
        .await;
        changes.try_recv().unwrap();
        let original = fs::read(&path).unwrap();
        let response = router(service.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/data/emacs/emacsclient_path")
                    .header("content-type", "application/json")
                    .body(Body::from(json!(directory.join("missing")).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["code"], "invalid-config");
        assert!(body["message"].as_str().unwrap().contains("文件不存在"));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            service
                .value("emacs", "emacsclient_path")
                .await
                .unwrap()
                .value,
            json!(executable)
        );
        assert!(changes.try_recv().is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
