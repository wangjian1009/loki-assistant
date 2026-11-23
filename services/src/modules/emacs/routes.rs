use std::sync::Arc;

use crate::http::error::{ErrorSource, response as api_error_response};
use crate::modules::network::remote::is_remote;
use crate::modules::status::protocol::{StatusResponse, StatusValues};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;

use super::{
    protocol::{
        API_VERSION, ActivationRequest, ActivationResponse, BuffersResponse, EvalRequest,
        EvalResponse, FullCaptureRequest, FullCaptureResponse, Notification, NotificationReceipt,
        NotificationResponse, NotificationStatusResponse, OpenAppRequest, OpenAppResponse,
    },
    service::{EmacsError, EmacsService},
};

pub fn router(service: Arc<EmacsService>) -> Router {
    crate::http::mount_routes(Router::new(), "emacs")
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024 + 4096))
        .with_state(service)
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "eval",
    method = "POST",
    path = "/emacs/eval",
    access = "paired",
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
    mount = "Arc<EmacsService>",
    id = "open-app",
    method = "POST",
    path = "/emacs/apps/open",
    access = "paired",
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
    mount = "Arc<EmacsService>",
    id = "activate-slack",
    method = "POST",
    path = "/emacs/slack/activate",
    summary = "激活 Emacs Slack 聊天",
    description = "从本机托盘激活最近使用的 Slack 聊天，或打开聊天选择器。"
)]
async fn activate_slack(
    State(service): State<Arc<EmacsService>>,
) -> Result<Json<OpenAppResponse>, Response> {
    service.activate_slack().await.map_err(error_response)?;
    Ok(Json(OpenAppResponse {
        api_version: API_VERSION.to_owned(),
        app: "slack".to_owned(),
        opened: true,
    }))
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "capture-full",
    method = "POST",
    path = "/emacs/capture/full",
    access = "paired",
    summary = "保存完整网页",
    description = "协议版本 1：接收浏览器转换后的完整 Org 文本，通过 loki-assistant-capture-full 进入 Emacs 捕获流程。"
)]
async fn capture_full(
    State(service): State<Arc<EmacsService>>,
    request: Result<Json<FullCaptureRequest>, JsonRejection>,
) -> Result<Response, Response> {
    let Json(request) = request.map_err(json_error)?;
    let result = service
        .capture_full(request)
        .await
        .map_err(error_response)?;
    let status = if result.status == "pending" {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(FullCaptureResponse {
            api_version: API_VERSION.to_owned(),
            status: result.status,
            url: result.url,
            document: result.document,
        }),
    )
        .into_response())
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "notification",
    method = "POST",
    path = "/emacs/receiver/notification",
    access = "paired",
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
    mount = "Arc<EmacsService>",
    id = "notification-status",
    method = "GET",
    path = "/emacs/receiver/notification/{id}",
    access = "paired",
    summary = "查询系统通知处理状态",
    description = "查询 unconfirmed 或 confirmed；未确认记录跨重启保留。"
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
    mount = "Arc<EmacsService>",
    id = "notification-receipt",
    method = "POST",
    path = "/emacs/receiver/notification/{id}/receipt",
    summary = "接收菜单栏应用的通知回执",
    description = "接收本机托盘回执。submitted 确认通知，failed 保留待确认记录。"
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
    mount = "Arc<EmacsService>",
    id = "activate",
    method = "POST",
    path = "/emacs/activate",
    access = "paired",
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
    mount = "Arc<EmacsService>",
    id = "buffers",
    method = "GET",
    path = "/emacs/buffers",
    access = "paired",
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
    mount = "Arc<EmacsService>",
    id = "receive-status",
    method = "POST",
    path = "/emacs/receiver/status",
    access = "paired",
    summary = "接收 Emacs 状态上报",
    description = "按状态元数据校验外部模块/数据项快照，整批通过后经共享 StatusService 发布；失败返回 400 与字段路径，不修改已有状态。"
)]
async fn receive_status(
    State(service): State<Arc<EmacsService>>,
    headers: HeaderMap,
    request: Result<Json<StatusValues>, JsonRejection>,
) -> Result<Json<StatusResponse>, Response> {
    let Json(values) = request.map_err(|_| {
        error_response(EmacsError::InvalidRequest(
            "状态请求格式无效，必须是模块到数据项的 JSON object".to_owned(),
        ))
    })?;
    service
        .receive_status_for(values, is_remote(&headers))
        .map(Json)
        .map_err(error_response)
}

fn json_error(error: JsonRejection) -> Response {
    error_response(EmacsError::InvalidRequest(error.body_text()))
}

pub(crate) fn error_response(error: EmacsError) -> Response {
    let (status, source, code) = match &error {
        EmacsError::InvalidRequest(_) => (
            StatusCode::BAD_REQUEST,
            ErrorSource::Services,
            "invalid-request",
        ),
        EmacsError::RemoteStatusForbidden(_) => (
            StatusCode::FORBIDDEN,
            ErrorSource::Business,
            "remote-status-forbidden",
        ),
        EmacsError::Config(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorSource::Services,
            "invalid-config",
        ),
        EmacsError::Process(_) | EmacsError::ConnectionFailed(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorSource::Emacs,
            "emacs-unavailable",
        ),
        EmacsError::CommandFailed { .. } => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Emacs,
            "emacsclient-failed",
        ),
        EmacsError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            ErrorSource::Emacs,
            "emacs-timeout",
        ),
        EmacsError::Lisp(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorSource::Emacs,
            "lisp-error",
        ),
        EmacsError::InvalidResponse(_) => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Emacs,
            "invalid-emacs-response",
        ),
        EmacsError::HandlerNotFound(_) => (
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "notification-handler-not-found",
        ),
        EmacsError::HandlerAlreadyRegistered(_) => (
            StatusCode::CONFLICT,
            ErrorSource::Business,
            "notification-handler-exists",
        ),
        EmacsError::Notification(_) => (
            StatusCode::BAD_GATEWAY,
            ErrorSource::Services,
            "notification-failed",
        ),
        EmacsError::SystemNotification(error) => {
            return crate::modules::notification::routes::error_response(error.clone());
        }
        EmacsError::BufferNotFound => (
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "buffer-not-found",
        ),
        EmacsError::AppNotFound(_) => (
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "app-not-found",
        ),
        EmacsError::Capture { code, .. } => match code.as_str() {
            "capture-not-installed" => (
                StatusCode::NOT_FOUND,
                ErrorSource::Business,
                "capture-not-installed",
            ),
            "capture-busy" => (StatusCode::CONFLICT, ErrorSource::Business, "capture-busy"),
            "capture-invalid" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorSource::Business,
                "capture-invalid",
            ),
            "emacs-ui-unavailable" => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorSource::Emacs,
                "emacs-ui-unavailable",
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorSource::Business,
                "capture-failed",
            ),
        },
    };
    api_error_response(status, source, code, error.to_string())
}
