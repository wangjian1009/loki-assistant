use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::Response,
};
use loki_services_macros::loki_api;

use super::{
    protocol::{
        API_VERSION, NotificationReceipt, NotificationRequest, NotificationRetryResponse,
        NotificationStatusResponse,
    },
    service::{NotificationError, NotificationService},
};
use crate::http::error::{ErrorSource, response};

pub fn router(service: Arc<NotificationService>) -> Router {
    crate::http::mount_routes(Router::new(), "notification").with_state(service)
}

#[loki_api(
    module = "notification",
    mount = "Arc<NotificationService>",
    id = "send",
    method = "POST",
    path = "/notification",
    access = "local",
    summary = "发送系统通知",
    description = "接收必填 title 和可选 body、app、callback_url，返回通知 ID 和当前确认状态。"
)]
async fn send(
    State(service): State<Arc<NotificationService>>,
    request: Result<Json<NotificationRequest>, JsonRejection>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    let Json(request) = request
        .map_err(|error| error_response(NotificationError::InvalidRequest(error.body_text())))?;
    service
        .send(request)
        .await
        .map(Json)
        .map_err(error_response)
}

#[loki_api(
    module = "notification",
    mount = "Arc<NotificationService>",
    id = "retry-pending",
    method = "POST",
    path = "/notification/retry-pending",
    access = "local",
    summary = "请求补发未确认通知",
    description = "本机托盘就绪后唤醒通知重试任务，使用原通知 ID 转交当前待确认通知。"
)]
async fn retry_pending(
    State(service): State<Arc<NotificationService>>,
) -> (StatusCode, Json<NotificationRetryResponse>) {
    service.request_pending_retry().await;
    (
        StatusCode::ACCEPTED,
        Json(NotificationRetryResponse {
            api_version: API_VERSION,
            status: "accepted",
        }),
    )
}

#[loki_api(
    module = "notification",
    mount = "Arc<NotificationService>",
    id = "status",
    method = "GET",
    path = "/notification/{id}",
    summary = "查询系统通知处理状态",
    description = "查询 unconfirmed 或 confirmed；未确认记录跨重启保留。"
)]
async fn status(
    State(service): State<Arc<NotificationService>>,
    Path(id): Path<String>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    service.status(&id).await.map(Json).map_err(error_response)
}

#[loki_api(
    module = "notification",
    mount = "Arc<NotificationService>",
    id = "receipt",
    method = "POST",
    path = "/notification/{id}/receipt",
    summary = "接收系统通知回执",
    description = "接收本机托盘回执。submitted 确认通知，failed 保留待确认记录。"
)]
async fn receipt(
    State(service): State<Arc<NotificationService>>,
    Path(id): Path<String>,
    request: Result<Json<NotificationReceipt>, JsonRejection>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    let Json(receipt) = request
        .map_err(|error| error_response(NotificationError::InvalidRequest(error.body_text())))?;
    service
        .receive_receipt(&id, receipt)
        .await
        .map(Json)
        .map_err(error_response)
}

pub(crate) fn error_response(error: NotificationError) -> Response {
    let (status, code, source) = match error {
        NotificationError::InvalidRequest(_) => (
            StatusCode::BAD_REQUEST,
            "invalid-request",
            ErrorSource::Services,
        ),
        NotificationError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "notification-storage-error",
            ErrorSource::Services,
        ),
        NotificationError::NotFound => (
            StatusCode::NOT_FOUND,
            "notification-not-found",
            ErrorSource::Business,
        ),
        NotificationError::ReceiptConflict => (
            StatusCode::CONFLICT,
            "notification-receipt-conflict",
            ErrorSource::Business,
        ),
        NotificationError::Capacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            "notification-capacity-exceeded",
            ErrorSource::Services,
        ),
    };
    response(status, source, code, error.to_string())
}
