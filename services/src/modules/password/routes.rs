use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;

use crate::http::error::{ErrorSource, response as api_error};
use crate::modules::config::service::SYSTEM_PASSWORD_PREFIX;

use super::{
    protocol::{API_VERSION, EntriesResponse, EntryResponse, ListRequest},
    service::{PasswordError, PasswordService},
};

pub(super) fn router(service: Arc<PasswordService>) -> Router {
    crate::http::mount_routes(Router::new(), "password")
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::map_response(no_store))
        .with_state(service)
}

async fn method_not_allowed() -> Response {
    api_error(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorSource::Services,
        "method-not-allowed",
        "密码接口仅提供 GET 查询。",
    )
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[loki_api(
    module = "password",
    mount = "Arc<PasswordService>",
    id = "entries",
    method = "GET",
    path = "/password/entries",
    summary = "查询密码条目",
    description = "协议版本 1：通过 pass ls 列出相对 .gpg 路径；可选 query 按路径进行不区分大小写的子串过滤，不解密条目。"
)]
async fn entries(
    State(service): State<Arc<PasswordService>>,
    request: Result<Query<ListRequest>, QueryRejection>,
) -> Result<Json<EntriesResponse>, Response> {
    let Query(request) = request.map_err(|_| error_response(PasswordError::InvalidRequest))?;
    let mut files = service
        .entries(&request.query)
        .await
        .map_err(error_response)?;
    files.retain(|file| !file.starts_with(SYSTEM_PASSWORD_PREFIX));
    Ok(Json(EntriesResponse {
        api_version: API_VERSION.to_owned(),
        files,
    }))
}

#[loki_api(
    module = "password",
    mount = "Arc<PasswordService>",
    id = "entry",
    method = "GET",
    path = "/password/entries/{*file}",
    summary = "读取单条密码原文",
    description = "协议版本 1：file 为路径中的相对 .gpg 文件名，支持多级目录；通过 pass show 返回完整 contents，不解析字段、不写入密码库。"
)]
async fn entry(
    State(service): State<Arc<PasswordService>>,
    file: Result<Path<String>, PathRejection>,
) -> Result<Json<EntryResponse>, Response> {
    let Path(file) = file.map_err(|_| error_response(PasswordError::InvalidPath))?;
    if file.starts_with(SYSTEM_PASSWORD_PREFIX) {
        return Err(error_response(PasswordError::EntryNotFound));
    }
    let contents = service.fetch(&file).await.map_err(error_response)?;
    Ok(Json(EntryResponse {
        api_version: API_VERSION.to_owned(),
        file,
        contents,
    }))
}

fn error_response(error: PasswordError) -> Response {
    let (status, source, code) = match &error {
        PasswordError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            ErrorSource::Business,
            "invalid-request",
        ),
        PasswordError::NotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorSource::Business,
            "password-not-configured",
        ),
        PasswordError::StoreUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorSource::Business,
            "password-store-unavailable",
        ),
        PasswordError::EntryNotFound => (
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "password-entry-not-found",
        ),
        PasswordError::InvalidPath => (
            StatusCode::BAD_REQUEST,
            ErrorSource::Business,
            "invalid-password-path",
        ),
        PasswordError::Process => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Services,
            "pass-unavailable",
        ),
        PasswordError::CommandFailed { .. } => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Services,
            "pass-command-failed",
        ),
        PasswordError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            ErrorSource::Services,
            "pass-timeout",
        ),
        PasswordError::InvalidOutput => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Services,
            "invalid-pass-response",
        ),
    };
    api_error(status, source, code, error.to_string()).into_response()
}
