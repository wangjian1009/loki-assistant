use super::{
    StatusService,
    protocol::{
        API_VERSION, StatusMetaResponse, StatusModuleResponse, StatusPathResponse, StatusResponse,
        StatusValueResponse,
    },
    service::StatusError,
};
use crate::{
    http::error::{ErrorSource, response},
    modules::network::remote::AuthenticatedPeer,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use loki_services_macros::loki_api;
use std::sync::Arc;

pub fn router(service: Arc<StatusService>) -> Router {
    crate::http::mount_routes(Router::new(), "status").with_state(service)
}

#[loki_api(
    module = "status",
    mount = "Arc<StatusService>",
    id = "meta",
    method = "GET",
    path = "/stats/meta",
    access = "paired",
    summary = "获取全部状态数据结构",
    description = "返回各模块静态声明的状态项、嵌套字段、类型、可空性和枚举值，不包含运行时数据；未发布状态也提供完整元数据。"
)]
async fn metadata(
    State(service): State<Arc<StatusService>>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Json<StatusMetaResponse> {
    Json(StatusMetaResponse {
        api_version: API_VERSION.to_owned(),
        modules: service.metadata_for(peer.is_some()),
    })
}

#[loki_api(
    module = "status",
    mount = "Arc<StatusService>",
    id = "snapshot",
    method = "GET",
    path = "/status",
    access = "paired",
    summary = "读取全部状态",
    description = "返回各发布方最后设置的模块/数据项快照；只读内存，不连接业务程序。"
)]
async fn snapshot(
    State(service): State<Arc<StatusService>>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Json<StatusResponse> {
    Json(service.snapshot_for(peer.is_some()).await)
}

#[loki_api(
    module = "status",
    mount = "Arc<StatusService>",
    id = "module-value",
    method = "GET",
    path = "/status/{module_id}",
    access = "paired",
    summary = "读取模块完整状态",
    description = "返回指定模块的完整状态对象及最后更新时间；service 对象直接包含 start_at，不增加节点层级。"
)]
async fn module_value(
    State(service): State<Arc<StatusService>>,
    Path(module): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Result<Json<StatusModuleResponse>, Response> {
    service
        .module_value_for(&module, peer.is_some())
        .await
        .map(Json)
        .map_err(error_response)
}

#[loki_api(
    module = "status",
    mount = "Arc<StatusService>",
    id = "value",
    method = "GET",
    path = "/status/{module_id}/{key}",
    access = "paired",
    summary = "读取指定状态",
    description = "返回指定模块/数据项的 value 与最后更新时间，未发布时返回 404 status-not-found。"
)]
async fn value(
    State(service): State<Arc<StatusService>>,
    Path((module, key)): Path<(String, String)>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Result<Json<StatusValueResponse>, Response> {
    service
        .value_for(&module, &key, peer.is_some())
        .await
        .map(Json)
        .map_err(error_response)
}

#[loki_api(
    module = "status",
    mount = "Arc<StatusService>",
    id = "path-value",
    method = "GET",
    path = "/status/{module_id}/{key}/{*path}",
    access = "paired",
    summary = "读取层级状态字段",
    description = "按 module/key/field/path 层级读取 object 或 array 中的字段，避免在状态接口路径和响应约定中使用点号。"
)]
async fn path_value(
    State(service): State<Arc<StatusService>>,
    Path((module, key, path)): Path<(String, String, String)>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> Result<Json<StatusPathResponse>, Response> {
    service
        .path_value_for(&module, &key, &path, peer.is_some())
        .await
        .map(Json)
        .map_err(error_response)
}

fn error_response(error: StatusError) -> Response {
    let (status, code) = match &error {
        StatusError::NotFound(..) => (StatusCode::NOT_FOUND, "status-not-found"),
        StatusError::RemoteAccessForbidden(..) => {
            (StatusCode::FORBIDDEN, "remote-status-forbidden")
        }
        _ => (StatusCode::BAD_REQUEST, "invalid-request"),
    };
    response(status, ErrorSource::Business, code, error.to_string())
}
