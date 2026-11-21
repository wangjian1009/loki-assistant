use std::sync::Arc;

use crate::{
    http::error::{ApiError, ErrorSource, response as api_error_response},
    modules::network::remote::is_remote,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{
        HeaderMap, StatusCode,
        header::{AUTHORIZATION, IF_NONE_MATCH},
    },
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde_json::Value;

use super::{
    model::{ConfigModuleMetadata, ConfigValue, ConfigValueSource},
    protocol::{
        MetaResponse, ModuleDataResponse, ModuleMetaResponse, RecordResponse, ValidateRequest,
        ValidateResponse, ValueResponse,
    },
    service::{ConfigService, ConfigServiceError},
};

pub fn router(service: Arc<ConfigService>) -> Router {
    crate::http::mount_routes(Router::new(), "config").with_state(service)
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "validate",
    method = "POST",
    path = "/config/validate",
    access = "paired",
    summary = "校验配置值",
    description = "校验 module_id、key 对应的 value，不修改配置；成功返回规范化 value，失败返回 valid=false 和错误说明。保存接口执行相同校验。"
)]
async fn validate(
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
    Json(request): Json<ValidateRequest>,
) -> Result<Json<ValidateResponse>, Response> {
    check_access(&service, &headers, &request.module_id, &request.key, true).await?;
    service
        .authorize_secret_request(&request.module_id, &request.key, bearer(&headers))
        .await
        .map_err(error_response)?;
    let (value, error) = match service
        .validate_value(&request.module_id, &request.key, &request.value)
        .await
    {
        Ok(value) => (Some(value), None),
        Err(error @ ConfigServiceError::InvalidValue(_)) => (
            None,
            Some(ApiError {
                source: ErrorSource::Business,
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
    mount = "Arc<ConfigService>",
    id = "meta",
    method = "GET",
    path = "/meta",
    access = "paired",
    summary = "获取完整配置模型",
    description = "返回所有配置模块及配置项类型、默认值和检查器描述。"
)]
async fn meta(State(service): State<Arc<ConfigService>>, headers: HeaderMap) -> Json<MetaResponse> {
    Json(MetaResponse {
        modules: visible_metadata(&service, &headers)
            .await
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "module-meta",
    method = "GET",
    path = "/meta/{module_id}",
    access = "paired",
    summary = "获取模块配置模型",
    description = "返回单个配置模块的完整描述。"
)]
async fn module_meta(
    Path(module_id): Path<String>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
) -> Result<Json<ModuleMetaResponse>, Response> {
    visible_metadata(&service, &headers)
        .await
        .into_iter()
        .find(|metadata| metadata.id == module_id)
        .map(|metadata| Json(metadata.into()))
        .ok_or_else(|| error_response(ConfigServiceError::ModuleNotFound(module_id)))
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "module-data",
    method = "GET",
    path = "/data/{module_id}",
    access = "paired",
    summary = "获取模块配置数据",
    description = "返回模块生效配置；每项包含 value 和 source，source 为 user 或 default。"
)]
async fn module_data(
    Path(module_id): Path<String>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
) -> Result<Json<ModuleDataResponse>, Response> {
    let remote = is_remote(&headers);
    let values = service
        .module_values_for(&module_id, remote)
        .await
        .map_err(error_response)?;
    let mut pending = service.module_pending_notifications(&module_id).await;
    if remote {
        let metadata = visible_metadata(&service, &headers).await;
        pending.retain(|key, _| {
            metadata
                .iter()
                .any(|module| module.id == module_id && module.item(key).is_some())
        });
    }
    Ok(Json(ModuleDataResponse {
        module_id,
        values,
        pending,
    }))
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "value",
    method = "GET",
    path = "/data/{module_id}/{key}",
    access = "paired",
    summary = "获取配置项",
    description = "返回单个生效配置的 value 和 source；用户设置优先，否则返回默认值。"
)]
async fn get_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
) -> Result<Json<ValueResponse>, Response> {
    check_access(&service, &headers, &module_id, &key, false).await?;
    let pending = service.pending_notifications(&module_id, &key).await;
    let data = match service.value(&module_id, &key).await {
        Ok(data) => data,
        Err(ConfigServiceError::ValueNotSet { .. }) if !pending.is_empty() => ConfigValue {
            value: Value::Null,
            source: ConfigValueSource::User,
        },
        Err(error) => return Err(error_response(error)),
    };
    Ok(Json(ValueResponse {
        module_id,
        key,
        data,
        pending,
    }))
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "set-value",
    method = "PUT",
    path = "/data/{module_id}/{key}",
    access = "paired",
    summary = "设置完整配置值",
    description = "替换一个配置项的完整值。"
)]
async fn put_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<Response, Response> {
    check_access(&service, &headers, &module_id, &key, true).await?;
    service
        .authorize_secret_request(&module_id, &key, bearer(&headers))
        .await
        .map_err(error_response)?;
    if service.has_notification(&module_id, &key) {
        let (value, pending) = service
            .notify_write(module_id.clone(), key.clone(), None, Some(value), false)
            .await
            .map_err(error_response)?
            .expect("写入值已有处理结果");
        return Ok((
            write_status(pending),
            Json(serde_json::json!({
                "module_id": module_id, "key": key, "value": value, "source": "user",
                "status": if pending { "pending" } else { "confirmed" },
            })),
        )
            .into_response());
    }
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
                pending: Vec::new(),
            })
            .into_response()
        })
        .map_err(error_response)
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "patch-value",
    method = "POST",
    path = "/data/{module_id}/{key}",
    access = "paired",
    summary = "部分更新配置值",
    description = "合并字典或追加列表项。"
)]
async fn post_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<Json<ValueResponse>, Response> {
    check_access(&service, &headers, &module_id, &key, true).await?;
    service
        .authorize_secret_request(&module_id, &key, bearer(&headers))
        .await
        .map_err(error_response)?;
    if service.has_notification(&module_id, &key) && service.notifications_attached() {
        return Err(error_response(ConfigServiceError::InvalidValue(
            "请逐条提交带通知的配置记录。".into(),
        )));
    }
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
                pending: Vec::new(),
            })
        })
        .map_err(error_response)
}

async fn visible_metadata(
    service: &ConfigService,
    headers: &HeaderMap,
) -> Vec<ConfigModuleMetadata> {
    let mut modules = service.metadata().await;
    if is_remote(headers) {
        for module in &mut modules {
            module.items.retain(|item| item.remote_access.allows_read());
        }
        modules.retain(|module| !module.items.is_empty());
    }
    modules
}

async fn check_access(
    service: &ConfigService,
    headers: &HeaderMap,
    module_id: &str,
    key: &str,
    write: bool,
) -> Result<(), Response> {
    if is_remote(headers) {
        service
            .require_remote_access(module_id, key, write)
            .await
            .map_err(error_response)?;
    }
    Ok(())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "delete-value",
    method = "DELETE",
    path = "/data/{module_id}/{key}",
    access = "paired",
    summary = "删除通知型单值配置",
    description = "通过配置项的通知接入点确认删除。"
)]
async fn delete_value(
    Path((module_id, key)): Path<(String, String)>,
    State(service): State<Arc<ConfigService>>,
    headers: HeaderMap,
) -> Result<StatusCode, Response> {
    check_access(&service, &headers, &module_id, &key, true).await?;
    if !service.has_notification(&module_id, &key) {
        return Err(error_response(ConfigServiceError::ActionNotFound));
    }
    service
        .authorize_secret_request(&module_id, &key, bearer(&headers))
        .await
        .map_err(error_response)?;
    let result = service
        .notify_write(module_id, key, None, None, false)
        .await
        .map_err(error_response)?;
    Ok(if result.is_some_and(|(_, pending)| pending) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::NO_CONTENT
    })
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "set-record",
    method = "PUT",
    path = "/data/{module_id}/{key}/{instance}",
    summary = "保存一条配置记录",
    description = "保存结构数组中的单条记录；携带 If-None-Match: * 时仅允许新增。"
)]
async fn put_record(
    State(service): State<Arc<ConfigService>>,
    Path((module_id, key, instance)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<Response, Response> {
    let create_only = match headers.get(IF_NONE_MATCH) {
        None => false,
        Some(header) if header == "*" => true,
        Some(_) => {
            return Err(error_response(ConfigServiceError::InvalidValue(
                "配置记录仅接受 If-None-Match: *。".into(),
            )));
        }
    };
    check_access(&service, &headers, &module_id, &key, true).await?;
    service
        .authorize_secret_request(&module_id, &key, bearer(&headers))
        .await
        .map_err(error_response)?;
    if service.has_notification(&module_id, &key) && service.notifications_attached() {
        let (value, pending) = service
            .notify_write(
                module_id.clone(),
                key.clone(),
                Some(instance.clone()),
                Some(value),
                create_only,
            )
            .await
            .map_err(error_response)?
            .expect("写入记录已有处理结果");
        return Ok((
            write_status(pending),
            Json(RecordResponse {
                module_id,
                key,
                instance,
                value,
                status: Some(if pending { "pending" } else { "confirmed" }),
            }),
        )
            .into_response());
    }
    let value = if create_only && service.has_record_creation_handler(&module_id, &key) {
        let config = Arc::clone(&service);
        let (module, key, instance) = (module_id.clone(), key.clone(), instance.clone());
        tokio::spawn(async move { config.prepare_record(&module, &key, &instance, value).await })
            .await
            .map_err(|_| error_response(ConfigServiceError::Storage("保存任务中断。".into())))?
    } else {
        service
            .replace_record(
                module_id.clone(),
                key.clone(),
                instance.clone(),
                value,
                create_only,
                false,
            )
            .await
    }
    .map_err(error_response)?;
    Ok(Json(RecordResponse {
        module_id,
        key,
        instance,
        value,
        status: None,
    })
    .into_response())
}

#[loki_api(
    module = "config",
    mount = "Arc<ConfigService>",
    id = "delete-record",
    method = "DELETE",
    path = "/data/{module_id}/{key}/{instance}",
    summary = "删除一条配置记录",
    description = "按结构数组元数据声明的主键删除记录。"
)]
async fn delete_record(
    State(service): State<Arc<ConfigService>>,
    Path((module_id, key, instance)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, Response> {
    check_access(&service, &headers, &module_id, &key, true).await?;
    service
        .authorize_secret_request(&module_id, &key, bearer(&headers))
        .await
        .map_err(error_response)?;
    if service.has_notification(&module_id, &key) && service.notifications_attached() {
        let result = service
            .notify_write(module_id, key, Some(instance), None, false)
            .await
            .map_err(error_response)?;
        return Ok(if result.is_some_and(|(_, pending)| pending) {
            StatusCode::ACCEPTED
        } else {
            StatusCode::NO_CONTENT
        });
    }
    service
        .remove_record(module_id, key, instance)
        .await
        .map_err(error_response)?;
    Ok(StatusCode::NO_CONTENT)
}

fn write_status(pending: bool) -> StatusCode {
    if pending {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    }
}

fn error_response(error: ConfigServiceError) -> Response {
    let status = match &error {
        ConfigServiceError::ModuleNotFound(_) | ConfigServiceError::ItemNotFound { .. } => {
            StatusCode::NOT_FOUND
        }
        ConfigServiceError::ValueNotSet { .. } => StatusCode::NOT_FOUND,
        ConfigServiceError::ActionNotFound => StatusCode::NOT_FOUND,
        ConfigServiceError::RecordExists => StatusCode::PRECONDITION_FAILED,
        ConfigServiceError::RecordAuthorizationRequired | ConfigServiceError::ChangePending => {
            StatusCode::CONFLICT
        }
        ConfigServiceError::InvalidValue(_) => StatusCode::BAD_REQUEST,
        ConfigServiceError::RemoteAccessForbidden { .. } => StatusCode::FORBIDDEN,
        ConfigServiceError::SecretUnauthorized => StatusCode::UNAUTHORIZED,
        ConfigServiceError::Storage(_) | ConfigServiceError::ChangeRejected { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    api_error_response(
        status,
        ErrorSource::Business,
        error_code(&error),
        error.to_string(),
    )
}

fn error_code(error: &ConfigServiceError) -> &'static str {
    match error {
        ConfigServiceError::ModuleNotFound(_) => "module-not-found",
        ConfigServiceError::ItemNotFound { .. } => "config-not-found",
        ConfigServiceError::ValueNotSet { .. } => "config-not-set",
        ConfigServiceError::InvalidValue(_) => "invalid-config",
        ConfigServiceError::RemoteAccessForbidden { .. } => "remote-config-forbidden",
        ConfigServiceError::SecretUnauthorized => "config-secret-auth-required",
        ConfigServiceError::ActionNotFound => "config-action-not-found",
        ConfigServiceError::RecordExists => "config-record-exists",
        ConfigServiceError::ChangePending => "config-change-pending",
        ConfigServiceError::RecordAuthorizationRequired => "config-record-authorization-required",
        ConfigServiceError::Storage(_) => "config-storage-error",
        ConfigServiceError::ChangeRejected { .. } => "config-change-rejected",
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
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
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
                    remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
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
                vec![crate::modules::emacs::config::EmacsConfig::metadata()],
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
        assert_eq!(body["error"]["code"], "invalid-config");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("文件不存在")
        );
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
