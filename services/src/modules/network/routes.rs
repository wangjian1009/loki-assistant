use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{BytesRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use loki_services_macros::loki_api;
use serde_json::json;

use super::{
    NetworkService,
    protocol::{API_VERSION, NetworkError},
};
use crate::http::error::{ErrorSource, response as api_error};

pub(super) fn router(service: Arc<NetworkService>) -> Router {
    Router::new()
        .route("/network/pairings", post(create))
        .route("/network/pairings/{id}", get(query).delete(cancel))
        .route("/network/peers", get(peers))
        .route(
            "/network/peers/{endpoint_id}",
            axum::routing::delete(remove_peer),
        )
        .method_not_allowed_fallback(|| async {
            api_error(
                StatusCode::METHOD_NOT_ALLOWED,
                ErrorSource::Services,
                "method-not-allowed",
                "该网络接口不支持此请求方法。",
            )
        })
        .layer(middleware::map_response(no_store))
        .layer(DefaultBodyLimit::max(1024))
        .with_state(service)
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error(error: NetworkError) -> Response {
    api_error(
        StatusCode::from_u16(error.status()).unwrap(),
        ErrorSource::Business,
        error.code(),
        error.to_string(),
    )
}

fn pairing_id(path: Result<Path<String>, PathRejection>) -> Result<String, Response> {
    let Path(id) = path.map_err(|_| error(NetworkError::InvalidRequest))?;
    uuid::Uuid::parse_str(&id).map_err(|_| error(NetworkError::InvalidRequest))?;
    Ok(id)
}

#[loki_api(
    module = "network",
    id = "create-pairing",
    method = "POST",
    path = "/network/pairings",
    summary = "创建绑定邀请",
    description = "协议版本 1：创建五分钟有效的单次邀请，返回二维码 PNG 的 Base64 与 payload，成功后取消原有未完成邀请；仅本机调用。"
)]
async fn create(
    State(service): State<Arc<NetworkService>>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Ok(body) = body else {
        return error(NetworkError::InvalidRequest);
    };
    if !body.is_empty()
        && serde_json::from_slice::<serde_json::Value>(&body).ok() != Some(json!({}))
    {
        return error(NetworkError::InvalidRequest);
    }
    match service.create_pairing().await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(reason) => error(reason),
    }
}

#[loki_api(
    module = "network",
    id = "pairing",
    method = "GET",
    path = "/network/pairings/{id}",
    summary = "查询绑定进度",
    description = "协议版本 1：读取邀请状态、有效期与绑定设备，不返回令牌。"
)]
async fn query(
    State(service): State<Arc<NetworkService>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<super::protocol::PairingResponse>, Response> {
    service
        .pairing(&pairing_id(path)?, false)
        .await
        .map(Json)
        .map_err(error)
}

#[loki_api(
    module = "network",
    id = "cancel-pairing",
    method = "DELETE",
    path = "/network/pairings/{id}",
    summary = "取消绑定邀请",
    description = "协议版本 1：取消未完成邀请；重复取消幂等，已完成绑定返回冲突。"
)]
async fn cancel(
    State(service): State<Arc<NetworkService>>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<super::protocol::PairingResponse>, Response> {
    service
        .pairing(&pairing_id(path)?, true)
        .await
        .map(Json)
        .map_err(error)
}

#[loki_api(
    module = "network",
    id = "peers",
    method = "GET",
    path = "/network/peers",
    summary = "查询绑定设备",
    description = "协议版本 1：返回持久化的设备身份、名称和绑定时间，不表示设备在线。"
)]
async fn peers(State(service): State<Arc<NetworkService>>) -> Response {
    match service.peers().await {
        Ok(peers) => Json(json!({"api_version": API_VERSION, "peers": peers})).into_response(),
        Err(reason) => error(reason),
    }
}

#[loki_api(
    module = "network",
    id = "remove-peer",
    method = "DELETE",
    path = "/network/peers/{endpoint_id}",
    summary = "解除设备绑定",
    description = "协议版本 1：删除持久化绑定并关闭该设备的当前连接；不存在时返回 404。"
)]
async fn remove_peer(
    State(service): State<Arc<NetworkService>>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let endpoint = match path
        .ok()
        .and_then(|Path(id)| id.parse::<iroh::EndpointId>().ok())
    {
        Some(endpoint) => endpoint,
        None => return error(NetworkError::InvalidRequest),
    };
    match service.remove_peer(endpoint).await {
        Ok(()) => Json(json!({"api_version": API_VERSION, "endpoint_id": endpoint.to_string(), "removed": true})).into_response(),
        Err(reason) => error(reason),
    }
}
