use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use loki_services_macros::loki_api;

use super::{
    protocol::{API_VERSION, ErrorResponse, NotificationReceipt, NotificationStatusResponse},
    service::{NotificationError, NotificationService},
};

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
    let (status, code) = match error {
        NotificationError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid-request"),
        NotificationError::Launch(_) => (StatusCode::BAD_GATEWAY, "notification-failed"),
        NotificationError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "notification-timeout"),
        NotificationError::NotFound => (StatusCode::NOT_FOUND, "notification-not-found"),
        NotificationError::ReceiptConflict => {
            (StatusCode::CONFLICT, "notification-receipt-conflict")
        }
        NotificationError::Capacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            "notification-capacity-exceeded",
        ),
    };
    (
        status,
        Json(ErrorResponse {
            api_version: API_VERSION.to_owned(),
            code: code.to_owned(),
            message: error.to_string(),
        }),
    )
        .into_response()
}
