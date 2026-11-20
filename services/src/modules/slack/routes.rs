use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        Path, Query, Request, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, HOST, LOCATION},
    },
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::{
    protocol::{Error, Page},
    service::SlackService,
};

type Service = State<Arc<SlackService>>;
type JsonInput<T> = Result<Json<T>, JsonRejection>;
type Params<T> = Result<Query<T>, QueryRejection>;
type Segment<T> = Result<Path<T>, PathRejection>;

pub(super) fn router(service: Arc<SlackService>) -> Router {
    let protected = crate::http::mount_routes(Router::new(), "team-authenticated").route_layer(
        middleware::from_fn_with_state(Arc::clone(&service), require_local),
    );
    crate::http::mount_routes(protected, "team-oauth")
        .method_not_allowed_fallback(|| async { Error::Method })
        .layer(middleware::map_response(
            |mut response: Response| async move {
                response
                    .headers_mut()
                    .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            },
        ))
        .with_state(service)
}

async fn require_local(
    State(service): Service,
    request: Request,
    next: Next,
) -> Result<Response, Error> {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    service.verify_local(token).await?;
    Ok(next.run(request).await)
}

fn header_uuid(headers: &HeaderMap, name: &'static str) -> Result<Uuid, Error> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(Error::Invalid)
}

fn session(headers: &HeaderMap) -> Result<Uuid, Error> {
    header_uuid(headers, "x-loki-slack-session")
}

fn location(status: StatusCode, path: String, value: Value) -> Response {
    (status, [(LOCATION, path)], Json(value)).into_response()
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "status",
    method = "GET",
    path = "/slack/status",
    summary = "读取 Slack 授权和连接状态",
    description = "读取 Slack 授权和连接状态"
)]
async fn status(State(service): Service) -> Json<Value> {
    Json(service.status().await)
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "create-authorization",
    method = "POST",
    path = "/slack/oauth/authorizations",
    summary = "创建 Slack 授权记录",
    description = "创建 Slack 授权记录"
)]
async fn authorize(State(service): Service) -> Result<Response, Error> {
    let (id, value) = service.begin_authorization().await?;
    Ok(location(
        StatusCode::CREATED,
        format!("/slack/oauth/authorizations/{id}"),
        value,
    ))
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "authorization",
    method = "GET",
    path = "/slack/oauth/authorizations/{id}",
    summary = "读取 Slack 授权记录",
    description = "读取 Slack 授权记录"
)]
async fn authorization(State(service): Service, path: Segment<Uuid>) -> Result<Json<Value>, Error> {
    service
        .authorization(path.map_err(|_| Error::Invalid)?.0)
        .await
        .map(Json)
}

#[derive(Deserialize)]
struct OAuthQuery {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-oauth",
    id = "oauth-callback",
    method = "GET",
    path = "/slack/oauth/callback",
    summary = "接收 Slack OAuth 浏览器回调",
    description = "接收 Slack OAuth 浏览器回调"
)]
async fn callback(
    State(service): Service,
    headers: HeaderMap,
    query: Params<OAuthQuery>,
) -> Result<Response, Error> {
    let query = query.map_err(|_| Error::OAuthState)?.0;
    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(Error::OAuthState)?;
    service
        .callback(host, &query.state, query.code, query.error.is_some())
        .await?;
    Ok((
        StatusCode::SEE_OTHER,
        [
            (LOCATION, "/slack/oauth/result"),
            (axum::http::header::REFERRER_POLICY, "no-referrer"),
        ],
    )
        .into_response())
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-oauth",
    id = "oauth-result",
    method = "GET",
    path = "/slack/oauth/result",
    summary = "显示 Slack 浏览器授权提示",
    description = "显示 Slack 浏览器授权提示"
)]
async fn oauth_result() -> Html<&'static str> {
    Html(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>Slack 授权</title><p>请返回 Emacs 查看 Slack 授权结果。</p></html>",
    )
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "disconnect",
    method = "DELETE",
    path = "/slack/session",
    summary = "断开本机 Slack 会话",
    description = "断开本机 Slack 会话"
)]
async fn disconnect(State(service): Service, headers: HeaderMap) -> Result<StatusCode, Error> {
    service.disconnect(session(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "conversations",
    method = "GET",
    path = "/slack/conversations",
    summary = "分页读取 Slack 私信会话",
    description = "分页读取 Slack 私信会话"
)]
async fn conversations(
    State(service): Service,
    headers: HeaderMap,
    page: Params<Page>,
) -> Result<Json<Value>, Error> {
    service
        .conversations(session(&headers)?, page.map_err(|_| Error::Invalid)?.0)
        .await
        .map(Json)
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "timeline",
    method = "GET",
    path = "/slack/conversations/{id}/messages",
    summary = "分页读取 Slack 顶层消息",
    description = "分页读取 Slack 顶层消息"
)]
async fn timeline(
    State(service): Service,
    headers: HeaderMap,
    id: Segment<String>,
    page: Params<Page>,
) -> Result<Json<Value>, Error> {
    service
        .messages(
            session(&headers)?,
            id.map_err(|_| Error::Invalid)?.0,
            None,
            page.map_err(|_| Error::Invalid)?.0,
        )
        .await
        .map(Json)
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "thread",
    method = "GET",
    path = "/slack/conversations/{id}/threads/{thread_ts}/messages",
    summary = "分页读取 Slack 线程",
    description = "分页读取 Slack 线程"
)]
async fn thread(
    State(service): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    page: Params<Page>,
) -> Result<Json<Value>, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    service
        .messages(
            session(&headers)?,
            id,
            Some(thread),
            page.map_err(|_| Error::Invalid)?.0,
        )
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadPosition {
    through_ts: String,
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "read-timeline",
    method = "PUT",
    path = "/slack/conversations/{id}/read-position",
    summary = "更新 Slack 会话已读位置",
    description = "更新 Slack 会话已读位置"
)]
async fn read_timeline(
    State(service): Service,
    headers: HeaderMap,
    id: Segment<String>,
    input: JsonInput<ReadPosition>,
) -> Result<Json<Value>, Error> {
    service
        .mark_read(
            session(&headers)?,
            id.map_err(|_| Error::Invalid)?.0,
            None,
            input.map_err(|_| Error::Invalid)?.0.through_ts,
        )
        .await
        .map(Json)
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "read-thread",
    method = "PUT",
    path = "/slack/conversations/{id}/threads/{thread_ts}/read-position",
    summary = "更新 Slack 线程已读位置",
    description = "更新 Slack 线程已读位置"
)]
async fn read_thread(
    State(service): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    input: JsonInput<ReadPosition>,
) -> Result<Json<Value>, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    service
        .mark_read(
            session(&headers)?,
            id,
            Some(thread),
            input.map_err(|_| Error::Invalid)?.0.through_ts,
        )
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    text: String,
}

async fn submit(
    service: Arc<SlackService>,
    headers: HeaderMap,
    id: String,
    thread: Option<String>,
    text: JsonInput<Text>,
) -> Result<Response, Error> {
    let key = header_uuid(&headers, "idempotency-key")?;
    let (pending, value) = service
        .enqueue(
            session(&headers)?,
            id,
            thread,
            key,
            text.map_err(|_| Error::Invalid)?.0.text,
        )
        .await?;
    let status = if pending {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok(location(
        status,
        format!("/slack/send-requests/{key}"),
        value,
    ))
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "send-message",
    method = "POST",
    path = "/slack/conversations/{id}/messages",
    summary = "提交 Slack 顶层消息",
    description = "提交 Slack 顶层消息"
)]
async fn send_message(
    State(service): Service,
    headers: HeaderMap,
    id: Segment<String>,
    text: JsonInput<Text>,
) -> Result<Response, Error> {
    submit(
        service,
        headers,
        id.map_err(|_| Error::Invalid)?.0,
        None,
        text,
    )
    .await
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "send-reply",
    method = "POST",
    path = "/slack/conversations/{id}/threads/{thread_ts}/messages",
    summary = "提交 Slack 线程回复",
    description = "提交 Slack 线程回复"
)]
async fn send_reply(
    State(service): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    text: JsonInput<Text>,
) -> Result<Response, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    submit(service, headers, id, Some(thread), text).await
}

#[loki_api(
    module = "slack",
    mount = "Arc<SlackService>",
    mount_group = "team-authenticated",
    id = "send-record",
    method = "GET",
    path = "/slack/send-requests/{request_id}",
    summary = "查询 Slack 发送记录",
    description = "查询 Slack 发送记录"
)]
async fn send_record(
    State(service): Service,
    headers: HeaderMap,
    path: Segment<Uuid>,
) -> Result<Json<Value>, Error> {
    service
        .send_record(session(&headers)?, path.map_err(|_| Error::Invalid)?.0)
        .await
        .map(Json)
}
