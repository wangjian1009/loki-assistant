use std::sync::Arc;

use crate::modules::status::protocol::{StatusResponse, StatusValues};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use loki_services_macros::loki_api;

use super::{
    protocol::{
        API_VERSION, ActivationRequest, ActivationResponse, BuffersResponse, ErrorResponse,
        EvalRequest, EvalResponse, Notification, NotificationReceipt, NotificationResponse,
        NotificationStatusResponse, OpenAppRequest, OpenAppResponse,
    },
    service::{EmacsError, EmacsService},
};

pub fn router(service: Arc<EmacsService>) -> Router {
    Router::new()
        .route("/emacs/eval", post(eval))
        .route("/emacs/receiver/status", post(receive_status))
        .route("/emacs/activate", post(activate))
        .route("/emacs/apps/open", post(open_app))
        .route("/emacs/receiver/notification", post(notification))
        .route(
            "/emacs/receiver/notification/{id}",
            get(notification_status),
        )
        .route(
            "/emacs/receiver/notification/{id}/receipt",
            post(notification_receipt),
        )
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
    id = "open-app",
    method = "POST",
    path = "/emacs/apps/open",
    summary = "打开 Emacs 应用",
    description = "协议版本 1：必填 app 查询参数，当前支持 config 和 logs；由服务打开并聚焦 Emacs 应用。应用未注册或未安装返回 404 app-not-found。"
)]
async fn open_app(
    State(service): State<Arc<EmacsService>>,
    request: Result<Query<OpenAppRequest>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<OpenAppResponse>, Response> {
    let Query(request) =
        request.map_err(|error| error_response(EmacsError::InvalidRequest(error.body_text())))?;
    service
        .open_app(&request.app)
        .await
        .map_err(error_response)?;
    Ok(Json(OpenAppResponse {
        api_version: API_VERSION.to_owned(),
        app: request.app,
        opened: true,
    }))
}

#[loki_api(
    module = "emacs",
    id = "notification",
    method = "POST",
    path = "/emacs/receiver/notification",
    summary = "接收 Emacs 通知上报",
    description = "协议版本 1：接收 title 和可选 app、body、buffer、module_id、payload；app 默认为 emacs，图标由 center 内置资源提供；默认转交菜单栏应用并返回 result.notification_id，可查询异步回执；点击仅激活 Emacs。"
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
    id = "notification-status",
    method = "GET",
    path = "/emacs/receiver/notification/{id}",
    summary = "查询系统通知处理状态",
    description = "协议版本 1：查询 forwarded、submitted、failed 或 unconfirmed；记录保留 15 分钟，services 重启后失效。"
)]
async fn notification_status(
    State(service): State<Arc<EmacsService>>,
    Path(id): Path<String>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    service
        .notification_status(&id)
        .await
        .map(Json)
        .map_err(error_response)
}

#[loki_api(
    module = "emacs",
    id = "notification-receipt",
    method = "POST",
    path = "/emacs/receiver/notification/{id}/receipt",
    summary = "接收菜单栏应用的通知回执",
    description = "协议版本 1：接收 submitted 或带 code、message 的 failed；相同回执幂等，冲突回执返回 409。"
)]
async fn notification_receipt(
    State(service): State<Arc<EmacsService>>,
    Path(id): Path<String>,
    request: Result<Json<NotificationReceipt>, JsonRejection>,
) -> Result<Json<NotificationStatusResponse>, Response> {
    let Json(receipt) = request.map_err(json_error)?;
    service
        .receive_notification_receipt(&id, receipt)
        .await
        .map(Json)
        .map_err(error_response)
}

#[loki_api(
    module = "emacs",
    id = "activate",
    method = "POST",
    path = "/emacs/activate",
    summary = "显示 Emacs 或指定 buffer",
    description = "协议版本 1：可选 buffer 查询参数；省略时打开或激活 Emacs，提供时定位存活 buffer，不自动启动新 daemon。"
)]
async fn activate(
    State(service): State<Arc<EmacsService>>,
    request: Result<Query<ActivationRequest>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<ActivationResponse>, Response> {
    let Query(request) =
        request.map_err(|error| error_response(EmacsError::InvalidRequest(error.body_text())))?;
    service
        .activate(request.buffer.as_deref())
        .await
        .map_err(error_response)?;
    Ok(Json(ActivationResponse {
        api_version: API_VERSION.to_owned(),
        activation: "emacs".to_owned(),
        buffer: request.buffer,
    }))
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

#[loki_api(
    module = "emacs",
    id = "receive-status",
    method = "POST",
    path = "/emacs/receiver/status",
    summary = "接收 Emacs 状态上报",
    description = "按状态元数据校验外部模块/数据项快照，整批通过后经共享 StatusService 发布；失败返回 400 与字段路径，不修改已有状态。"
)]
async fn receive_status(
    State(service): State<Arc<EmacsService>>,
    request: Result<Json<StatusValues>, JsonRejection>,
) -> Result<Json<StatusResponse>, Response> {
    let Json(values) = request.map_err(|_| {
        error_response(EmacsError::InvalidRequest(
            "状态请求格式无效，必须是模块到数据项的 JSON object".to_owned(),
        ))
    })?;
    service
        .receive_status(values)
        .map(Json)
        .map_err(error_response)
}

fn json_error(error: JsonRejection) -> Response {
    error_response(EmacsError::InvalidRequest(error.body_text()))
}

fn error_response(error: EmacsError) -> Response {
    let (status, code) = match &error {
        EmacsError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, "invalid-request"),
        EmacsError::Config(_) => (StatusCode::INTERNAL_SERVER_ERROR, "invalid-config"),
        EmacsError::Process(_) | EmacsError::ConnectionFailed(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "emacs-unavailable")
        }
        EmacsError::CommandFailed { .. } => (StatusCode::BAD_GATEWAY, "emacsclient-failed"),
        EmacsError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "emacs-timeout"),
        EmacsError::Lisp(_) => (StatusCode::UNPROCESSABLE_ENTITY, "lisp-error"),
        EmacsError::InvalidResponse(_) => (StatusCode::BAD_GATEWAY, "invalid-emacs-response"),
        EmacsError::HandlerNotFound(_) => (StatusCode::NOT_FOUND, "notification-handler-not-found"),
        EmacsError::HandlerAlreadyRegistered(_) => {
            (StatusCode::CONFLICT, "notification-handler-exists")
        }
        EmacsError::Notification(_) => (StatusCode::BAD_GATEWAY, "notification-failed"),
        EmacsError::SystemNotification(error) => {
            return crate::modules::notification::routes::error_response(error.clone());
        }
        EmacsError::BufferNotFound => (StatusCode::NOT_FOUND, "buffer-not-found"),
        EmacsError::AppNotFound(_) => (StatusCode::NOT_FOUND, "app-not-found"),
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
