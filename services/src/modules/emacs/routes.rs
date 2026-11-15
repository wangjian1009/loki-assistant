use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use loki_services_macros::loki_api;

use super::{
    protocol::{
        API_VERSION, BuffersResponse, ErrorResponse, EvalRequest, EvalResponse, Notification,
        NotificationResponse,
    },
    service::{EmacsError, EmacsService},
};

pub fn router(service: Arc<EmacsService>) -> Router {
    Router::new()
        .route("/emacs/eval", post(eval))
        .route("/emacs/receiver/notification", post(notification))
        .route("/emacs/buffers", get(buffers))
        .with_state(service)
}

#[loki_api(
    module = "emacs",
    id = "eval",
    method = "POST",
    path = "/emacs/eval",
    summary = "执行 Emacs Lisp",
    description = "协议版本 1：输入 script，返回 Lisp 打印表示 result；server 未运行时通过 emacsclient 自动启动。"
)]
async fn eval(
    State(service): State<Arc<EmacsService>>,
    request: Result<Json<EvalRequest>, JsonRejection>,
) -> Result<Json<EvalResponse>, Response> {
    let Json(request) = request.map_err(json_error)?;
    service
        .eval(&request.script)
        .await
        .map(|result| {
            Json(EvalResponse {
                api_version: API_VERSION.to_owned(),
                result,
            })
        })
        .map_err(error_response)
}

#[loki_api(
    module = "emacs",
    id = "notification",
    method = "POST",
    path = "/emacs/receiver/notification",
    summary = "接收 Emacs 通知上报",
    description = "协议版本 1：接收 title、icon、buffer 和可选 module_id、payload；默认转交菜单栏应用展示 macOS 系统通知，点击仅激活 Emacs。"
)]
async fn notification(
    State(service): State<Arc<EmacsService>>,
    request: Result<Json<Notification>, JsonRejection>,
) -> Result<Json<NotificationResponse>, Response> {
    let Json(request) = request.map_err(json_error)?;
    let module_id = request.module_id.clone();
    service
        .notify(request)
        .await
        .map(|result| {
            Json(NotificationResponse {
                api_version: API_VERSION.to_owned(),
                module_id,
                result,
            })
        })
        .map_err(error_response)
}

#[loki_api(
    module = "emacs",
    id = "buffers",
    method = "GET",
    path = "/emacs/buffers",
    summary = "获取 Emacs buffer 列表",
    description = "协议版本 1：返回 buffer 的 name、file、modified 和 major_mode。"
)]
async fn buffers(
    State(service): State<Arc<EmacsService>>,
) -> Result<Json<BuffersResponse>, Response> {
    service
        .buffers()
        .await
        .map(|buffers| {
            Json(BuffersResponse {
                api_version: API_VERSION.to_owned(),
                buffers,
            })
        })
        .map_err(error_response)
}

fn json_error(error: JsonRejection) -> Response {
    error_response(EmacsError::InvalidRequest(error.body_text()))
}

fn error_response(error: EmacsError) -> Response {
    let (status, code) = match &error {
        EmacsError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid-request"),
        EmacsError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "invalid-config"),
        EmacsError::Process(_) => (StatusCode::SERVICE_UNAVAILABLE, "emacs-unavailable"),
        EmacsError::CommandFailed { .. } => (StatusCode::BAD_GATEWAY, "emacsclient-failed"),
        EmacsError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "emacs-timeout"),
        EmacsError::Lisp(_) => (StatusCode::UNPROCESSABLE_ENTITY, "lisp-error"),
        EmacsError::InvalidResponse(_) => (StatusCode::BAD_GATEWAY, "invalid-emacs-response"),
        EmacsError::HandlerNotFound(_) => (StatusCode::NOT_FOUND, "notification-handler-not-found"),
        EmacsError::HandlerAlreadyRegistered(_) => {
            (StatusCode::CONFLICT, "notification-handler-exists")
        }
        EmacsError::Notification(_) => (StatusCode::BAD_GATEWAY, "notification-failed"),
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
