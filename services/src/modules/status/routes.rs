use super::{
    StatusService,
    protocol::{
        API_VERSION, ErrorResponse, StatusMetaResponse, StatusResponse, StatusValueResponse,
    },
    service::StatusError,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use loki_services_macros::loki_api;
use std::sync::Arc;

pub fn router(service: Arc<StatusService>) -> Router {
    Router::new()
        .route("/stats/meta", get(metadata))
        .route("/status", get(snapshot))
        .route("/status/{module_id}/{key}", get(value))
        .with_state(service)
}

#[loki_api(
    module = "status",
    id = "meta",
    method = "GET",
    path = "/stats/meta",
    summary = "获取全部状态数据结构",
    description = "返回各模块静态声明的状态项、嵌套字段、类型、可空性和枚举值，不包含运行时数据；未发布状态也提供完整元数据。"
)]
async fn metadata(State(service): State<Arc<StatusService>>) -> Json<StatusMetaResponse> {
    Json(StatusMetaResponse {
        api_version: API_VERSION.to_owned(),
        modules: service.metadata(),
    })
}

#[loki_api(
    module = "status",
    id = "snapshot",
    method = "GET",
    path = "/status",
    summary = "读取全部状态",
    description = "返回各发布方最后设置的模块/数据项快照；只读内存，不连接业务程序。"
)]
async fn snapshot(State(service): State<Arc<StatusService>>) -> Json<StatusResponse> {
    Json(service.snapshot())
}

#[loki_api(
    module = "status",
    id = "value",
    method = "GET",
    path = "/status/{module_id}/{key}",
    summary = "读取指定状态",
    description = "返回指定模块/数据项的 value 与最后更新时间，未发布时返回 404 status-not-found。"
)]
async fn value(
    State(service): State<Arc<StatusService>>,
    Path((module, key)): Path<(String, String)>,
) -> Result<Json<StatusValueResponse>, Response> {
    service
        .value(&module, &key)
        .map(Json)
        .map_err(error_response)
}

fn error_response(error: StatusError) -> Response {
    let (status, code) = match &error {
        StatusError::NotFound(..) => (StatusCode::NOT_FOUND, "status-not-found"),
        _ => (StatusCode::BAD_REQUEST, "invalid-request"),
    };
    response(status, code, error.to_string())
}
fn response(status: StatusCode, code: &str, message: String) -> Response {
    (
        status,
        Json(ErrorResponse {
            api_version: API_VERSION.to_owned(),
            code: code.to_owned(),
            message,
        }),
    )
        .into_response()
}
