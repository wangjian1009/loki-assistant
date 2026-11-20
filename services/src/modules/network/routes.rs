use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{BytesRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde_json::json;

use super::{
    NetworkService,
    protocol::{API_VERSION, NetworkError},
};
use crate::http::error::{ErrorSource, response as api_error};

pub(super) fn router(service: Arc<NetworkService>) -> Router {
    crate::http::mount_routes(Router::new(), "network")
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

#[loki_api(
    module = "network",
    mount = "Arc<NetworkService>",
    id = "session",
    method = "GET",
    path = "/network/session",
    access = "paired",
    summary = "查询已认证服务会话",
    description = "向已绑定手机返回当前电脑身份、在线状态与绑定时间，认证身份由 iroh 分发器注入。"
)]
async fn session(
    State(service): State<Arc<NetworkService>>,
    peer: Option<Extension<super::remote::AuthenticatedPeer>>,
) -> Response {
    let Some(Extension(peer)) = peer else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            ErrorSource::Services,
            "peer-authentication-required",
            "此接口需要已认证的远端身份",
        );
    };
    match service.peer_session(peer.0).await {
        Ok(value) => Json(value).into_response(),
        Err(reason) => error(reason),
    }
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
    mount = "Arc<NetworkService>",
    id = "create-pairing",
    method = "POST",
    path = "/network/pairings",
    access = "paired",
    summary = "创建绑定邀请",
    description = "协议版本 1，创建五分钟有效的单次邀请，返回二维码 PNG 的 Base64 与 payload，成功后取消原有未完成邀请。"
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
    mount = "Arc<NetworkService>",
    id = "pairing",
    method = "GET",
    path = "/network/pairings/{id}",
    access = "paired",
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
    mount = "Arc<NetworkService>",
    id = "cancel-pairing",
    method = "DELETE",
    path = "/network/pairings/{id}",
    access = "paired",
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
    mount = "Arc<NetworkService>",
    id = "peers",
    method = "GET",
    path = "/network/peers",
    access = "paired",
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
    mount = "Arc<NetworkService>",
    id = "remove-peer",
    method = "DELETE",
    path = "/network/peers/{endpoint_id}",
    access = "paired",
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
