use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, State,
        rejection::{BytesRejection, JsonRejection, PathRejection},
    },
    http::StatusCode,
    response::Response,
};
use loki_services_macros::loki_api;

use super::{
    AlertError, AlertInput, AlertService, MODULE_ID,
    protocol::{API_VERSION, PublishAlertResponse},
};
use crate::http::error::{ErrorSource, response};

pub(super) fn router(service: Arc<AlertService>) -> Router {
    crate::http::mount_routes(Router::new(), MODULE_ID).with_state(service)
}

#[loki_api(
    module = "alert",
    mount = "Arc<AlertService>",
    id = "publish",
    method = "PUT",
    path = "/alert/{id}",
    access = "local",
    summary = "发布告警",
    description = "按调用方提供的 ID 新增或完整替换告警。新增和内容变更触发系统通知。"
)]
async fn publish(
    State(service): State<Arc<AlertService>>,
    path: Result<Path<String>, PathRejection>,
    request: Result<Json<AlertInput>, JsonRejection>,
) -> Result<(StatusCode, Json<PublishAlertResponse>), Response> {
    let Path(id) = path.map_err(|_| invalid_request("无效的告警 ID"))?;
    let Json(input) = request.map_err(|_| invalid_request("告警请求体必须符合 JSON 字段约定"))?;
    let (alert, created) = service
        .publish_with_creation(&id, input)
        .await
        .map_err(error_response)?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(PublishAlertResponse {
            api_version: API_VERSION,
            alert,
        }),
    ))
}

#[loki_api(
    module = "alert",
    mount = "Arc<AlertService>",
    id = "clear",
    method = "DELETE",
    path = "/alert/{id}",
    access = "local",
    summary = "清理告警",
    description = "按 ID 清理活跃告警，重复清理同一 ID 也返回成功。"
)]
async fn clear(
    State(service): State<Arc<AlertService>>,
    path: Result<Path<String>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, Response> {
    let Path(id) = path.map_err(|_| invalid_request("无效的告警 ID"))?;
    if !body
        .map_err(|_| invalid_request("无法读取请求体"))?
        .is_empty()
    {
        return Err(invalid_request("清理告警时请求体必须为空"));
    }
    service.clear(&id).await.map_err(error_response)?;
    Ok(StatusCode::NO_CONTENT)
}

fn invalid_request(message: &'static str) -> Response {
    response(
        StatusCode::BAD_REQUEST,
        ErrorSource::Business,
        "invalid-request",
        message,
    )
}

fn error_response(error: AlertError) -> Response {
    match error {
        AlertError::InvalidRequest(message) => invalid_request(message),
        AlertError::Status(_) => response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorSource::Services,
            "alert-status-error",
            error.to_string(),
        ),
    }
}
