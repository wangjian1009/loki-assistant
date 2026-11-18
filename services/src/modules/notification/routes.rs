use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use loki_services_macros::loki_api;

use super::{
    protocol::{NotificationReceipt, NotificationStatusResponse},
    service::{NotificationError, NotificationService},
};
use crate::http::error::{ErrorSource, response};

pub fn router(service: Arc<NotificationService>) -> Router {
    Router::new()
        .route("/notification/{id}", get(status))
        .route("/notification/{id}/receipt", post(receipt))
        .with_state(service)
}

#[loki_api(
    module = "notification",
    id = "status",
    method = "GET",
    path = "/notification/{id}",
    summary = "查询系统通知处理状态",
    description = "协议版本 1：查询 forwarded、submitted、failed 或 unconfirmed；记录保留 15 分钟，services 重启后失效。"
)]
async fn status(
    State(service): State<Arc<NotificationService>>,
    Path(id): Path<String>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    service.status(&id).await.map(Json).map_err(error_response)
}

#[loki_api(
    module = "notification",
    id = "receipt",
    method = "POST",
    path = "/notification/{id}/receipt",
    summary = "接收系统通知回执",
    description = "协议版本 1：接收 submitted 或带 code、message 的 failed；相同回执幂等，冲突返回 409。"
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
        NotificationError::Launch(_) => (
            StatusCode::BAD_GATEWAY,
            "notification-failed",
            ErrorSource::Services,
        ),
        NotificationError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "notification-timeout",
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
