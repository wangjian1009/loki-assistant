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
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    projects::Projects,
    protocol::{Error, Page},
    service::SlackService,
};

type Service = State<Arc<Projects>>;
type JsonInput<T> = Result<Json<T>, JsonRejection>;
type Params<T> = Result<Query<T>, QueryRejection>;
type Segment<T> = Result<Path<T>, PathRejection>;

pub(super) fn router(service: Arc<Projects>) -> Router {
    let protected = crate::http::mount_routes(Router::new(), "slack-authenticated").route_layer(
        middleware::from_fn_with_state(Arc::clone(&service), require_local),
    );
    crate::http::mount_routes(protected, "slack-local")
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

pub(super) fn config_notification_router(projects: Arc<Projects>) -> Router {
    use axum::routing::post;
    Router::new()
        .route(
            "/slack/config/teams/{team_id}",
            post(team_config_changed)
                .get(team_config_status)
                .delete(team_config_changed),
        )
        .with_state(projects)
}

async fn team_config_changed(
    State(projects): Service,
    axum::Extension(request): axum::Extension<crate::modules::config::service::NotificationRequest>,
    Path(team_id): Path<String>,
) -> Result<StatusCode, Error> {
    validate_team_id(&team_id)?;
    if request.deleted {
        sync_after_confirmation(projects, request);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Ok(StatusCode::ACCEPTED)
    }
}

async fn team_config_status(
    State(projects): Service,
    axum::Extension(request): axum::Extension<crate::modules::config::service::NotificationRequest>,
    Path(team_id): Path<String>,
) -> Result<Json<Value>, Error> {
    validate_team_id(&team_id)?;
    let status = projects.registration_status(request.id, &team_id).await;
    if status == "confirmed" {
        sync_after_confirmation(projects, request);
    }
    Ok(Json(json!({"status": status})))
}

fn sync_after_confirmation(
    projects: Arc<Projects>,
    request: crate::modules::config::service::NotificationRequest,
) {
    tokio::spawn(async move {
        if request.confirmed().await
            && let Err(error) = projects.sync_projects().await
        {
            eprintln!("同步 Slack 工作区失败：{error}");
        }
    });
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
    service.verify_local(token)?;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    team_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    team_id: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    unread: Option<bool>,
    kind: Option<String>,
}

impl PageQuery {
    fn parse(query: Params<Self>) -> Result<(Option<String>, Page), Error> {
        let Self {
            team_id,
            limit,
            cursor,
            unread,
            kind,
        } = query.map_err(|_| Error::Invalid)?.0;
        if let Some(id) = &team_id {
            validate_team_id(id)?;
        }
        Ok((
            team_id,
            Page {
                limit,
                cursor,
                unread,
                kind,
            },
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessagePage {
    limit: Option<usize>,
    cursor: Option<String>,
}

impl From<MessagePage> for Page {
    fn from(query: MessagePage) -> Self {
        Self {
            limit: query.limit,
            cursor: query.cursor,
            ..Self::default()
        }
    }
}

fn validate_team_id(team_id: &str) -> Result<(), Error> {
    if super::projects::valid_team_id(team_id) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

async fn scoped(
    projects: &Projects,
    scope: Params<Scope>,
) -> Result<(String, Arc<SlackService>), Error> {
    let Scope { team_id } = scope.map_err(|_| Error::Invalid)?.0;
    validate_team_id(&team_id)?;
    let service = projects.service(&team_id).await?;
    Ok((team_id, service))
}

async fn conversation(
    projects: &Projects,
    composite_id: String,
) -> Result<(String, String, Arc<SlackService>), Error> {
    let (team_id, id) = composite_id.split_once('.').ok_or(Error::Invalid)?;
    validate_team_id(team_id)?;
    if !super::protocol::valid_id(id) {
        return Err(Error::Invalid);
    }
    let service = projects.service(team_id).await?;
    Ok((team_id.to_owned(), id.to_owned(), service))
}

fn conversation_result(mut value: Value, team_id: &str) -> Value {
    if let Some(id) = value["conversation_id"].as_str() {
        value["conversation_id"] = json!(format!("{team_id}.{id}"));
    }
    identified(value, team_id)
}

fn identified(mut value: Value, team_id: &str) -> Value {
    value["team_id"] = json!(team_id);
    value
}

fn messages_result(mut value: Value, team_id: &str) -> Value {
    fn qualify_author(message: &mut Value, team_id: &str) {
        if let Some(id) = message["author"]["id"].as_str() {
            message["author"]["id"] = json!(format!("{team_id}.{id}"));
        }
    }

    if let Some(messages) = value["items"].as_array_mut() {
        for message in messages {
            qualify_author(message, team_id);
        }
    }
    if let Some(root) = value.get_mut("root") {
        qualify_author(root, team_id);
    }
    conversation_result(value, team_id)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "token-import-page",
    method = "GET",
    path = "/slack/token-import",
    summary = "打开 Slack 令牌导入页面",
    description = "显示已有 Slack 用户令牌的导入说明。"
)]
async fn token_import_page() -> Response {
    (
        [(axum::http::header::REFERRER_POLICY, "no-referrer"),
         (axum::http::header::CONTENT_SECURITY_POLICY, "default-src 'none'; base-uri 'none'; frame-ancestors 'none'")],
        Html(concat!(
            "<!doctype html><html lang=zh-CN><meta charset=utf-8>",
            "<title>导入 Slack 用户令牌</title><h1>导入 Slack 用户令牌</h1>",
            "<p>打开 Loki Assistant Chrome 插件，粘贴目标工作区已有的 xoxp- 用户令牌并点击「接受」。</p>"
        )),
    ).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenImport {
    team_id: String,
    token: String,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "accept-token-import",
    method = "POST",
    path = "/slack/token-import",
    summary = "接受 Slack 用户令牌",
    description = "凭一次性导入标识提交令牌，校验所属工作区后保存配置。"
)]
async fn accept_token_import(
    State(projects): Service,
    headers: HeaderMap,
    input: JsonInput<TokenImport>,
) -> Result<StatusCode, Error> {
    let attempt = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(Error::ImportExpired)?;
    let Json(TokenImport { team_id, token }) = input.map_err(|_| Error::Invalid)?;
    validate_team_id(&team_id)?;
    tokio::spawn(async move { projects.accept_token(&team_id, attempt, token).await })
        .await
        .map_err(|_| Error::Unavailable)??;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "create-authorization",
    method = "POST",
    path = "/slack/oauth/authorizations",
    summary = "创建 Slack 授权记录",
    description = "创建 Slack 授权记录"
)]
async fn authorize(State(projects): Service, scope: Params<Scope>) -> Result<Response, Error> {
    let (team_id, service) = scoped(&projects, scope).await?;
    let (id, value) = service.begin_authorization().await?;
    Ok(location(
        StatusCode::CREATED,
        format!("/slack/oauth/authorizations/{id}?team_id={team_id}"),
        identified(value, &team_id),
    ))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "authorization",
    method = "GET",
    path = "/slack/oauth/authorizations/{id}",
    summary = "读取 Slack 授权记录",
    description = "读取 Slack 授权记录"
)]
async fn authorization(
    State(projects): Service,
    path: Segment<Uuid>,
    scope: Params<Scope>,
) -> Result<Json<Value>, Error> {
    let (team_id, service) = scoped(&projects, scope).await?;
    service
        .authorization(path.map_err(|_| Error::Invalid)?.0)
        .await
        .map(|value| Json(identified(value, &team_id)))
}

#[derive(Deserialize)]
struct OAuthQuery {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
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
        .ok_or(Error::OAuthState)?
        .to_owned();
    // 浏览器关闭后仍需完成已经收到的一次性 OAuth 回调。
    tokio::spawn(async move {
        service
            .callback(&host, &query.state, query.code, query.error.is_some())
            .await
    })
    .await
    .map_err(|_| Error::Unavailable)??;
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
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "oauth-result",
    method = "GET",
    path = "/slack/oauth/result",
    summary = "显示 Slack 浏览器授权提示",
    description = "显示 Slack 浏览器授权提示"
)]
async fn oauth_result() -> Html<&'static str> {
    Html(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>Slack 授权</title><p>请返回配置界面或 Slack 客户端查看授权结果。</p></html>",
    )
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "disconnect",
    method = "DELETE",
    path = "/slack/session",
    summary = "断开本机 Slack 会话",
    description = "断开本机 Slack 会话"
)]
async fn disconnect(
    State(projects): Service,
    headers: HeaderMap,
    scope: Params<Scope>,
) -> Result<StatusCode, Error> {
    let (_, service) = scoped(&projects, scope).await?;
    service.disconnect(session(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "conversations",
    method = "GET",
    path = "/slack/conversations",
    summary = "分页读取 Slack 聊天",
    description = "分页读取 Slack 聊天"
)]
async fn conversations(
    State(projects): Service,
    query: Params<PageQuery>,
) -> Result<Json<Value>, Error> {
    let (team_id, page) = PageQuery::parse(query)?;
    projects
        .conversations(team_id.as_deref(), page)
        .await
        .map(Json)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "users",
    method = "GET",
    path = "/slack/users",
    summary = "分页读取 Slack 用户",
    description = "合并已授权工作区的用户目录。"
)]
async fn users(State(projects): Service, query: Params<MessagePage>) -> Result<Json<Value>, Error> {
    projects
        .users(query.map_err(|_| Error::Invalid)?.0.into())
        .await
        .map(Json)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "timeline",
    method = "GET",
    path = "/slack/conversations/{id}/messages",
    summary = "分页读取 Slack 顶层消息",
    description = "分页读取 Slack 顶层消息"
)]
async fn timeline(
    State(projects): Service,
    headers: HeaderMap,
    id: Segment<String>,
    query: Params<MessagePage>,
) -> Result<Json<Value>, Error> {
    let page = query.map_err(|_| Error::Invalid)?.0.into();
    let (team_id, id, service) = conversation(&projects, id.map_err(|_| Error::Invalid)?.0).await?;
    service
        .messages(session(&headers)?, id, None, page)
        .await
        .map(|value| Json(messages_result(value, &team_id)))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "thread",
    method = "GET",
    path = "/slack/conversations/{id}/threads/{thread_ts}/messages",
    summary = "分页读取 Slack 线程",
    description = "分页读取 Slack 线程"
)]
async fn thread(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    query: Params<MessagePage>,
) -> Result<Json<Value>, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    let (team_id, id, service) = conversation(&projects, id).await?;
    let page = query.map_err(|_| Error::Invalid)?.0.into();
    service
        .messages(session(&headers)?, id, Some(thread), page)
        .await
        .map(|value| Json(messages_result(value, &team_id)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadPosition {
    through_ts: String,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "read-timeline",
    method = "PUT",
    path = "/slack/conversations/{id}/read-position",
    summary = "更新 Slack 会话已读位置",
    description = "更新 Slack 会话已读位置"
)]
async fn read_timeline(
    State(projects): Service,
    headers: HeaderMap,
    id: Segment<String>,
    input: JsonInput<ReadPosition>,
) -> Result<Json<Value>, Error> {
    let (team_id, id, service) = conversation(&projects, id.map_err(|_| Error::Invalid)?.0).await?;
    service
        .mark_read(
            session(&headers)?,
            id,
            None,
            input.map_err(|_| Error::Invalid)?.0.through_ts,
        )
        .await
        .map(|value| Json(conversation_result(value, &team_id)))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "read-thread",
    method = "PUT",
    path = "/slack/conversations/{id}/threads/{thread_ts}/read-position",
    summary = "更新 Slack 线程已读位置",
    description = "更新 Slack 线程已读位置"
)]
async fn read_thread(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    input: JsonInput<ReadPosition>,
) -> Result<Json<Value>, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    let (team_id, id, service) = conversation(&projects, id).await?;
    service
        .mark_read(
            session(&headers)?,
            id,
            Some(thread),
            input.map_err(|_| Error::Invalid)?.0.through_ts,
        )
        .await
        .map(|value| Json(conversation_result(value, &team_id)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    text: String,
}

async fn submit(
    service: Arc<SlackService>,
    team_id: &str,
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
        format!("/slack/send-requests/{key}?team_id={team_id}"),
        conversation_result(value, team_id),
    ))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "send-message",
    method = "POST",
    path = "/slack/conversations/{id}/messages",
    summary = "提交 Slack 顶层消息",
    description = "提交 Slack 顶层消息"
)]
async fn send_message(
    State(projects): Service,
    headers: HeaderMap,
    id: Segment<String>,
    text: JsonInput<Text>,
) -> Result<Response, Error> {
    let (team_id, id, service) = conversation(&projects, id.map_err(|_| Error::Invalid)?.0).await?;
    submit(service, &team_id, headers, id, None, text).await
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "send-reply",
    method = "POST",
    path = "/slack/conversations/{id}/threads/{thread_ts}/messages",
    summary = "提交 Slack 线程回复",
    description = "提交 Slack 线程回复"
)]
async fn send_reply(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, String)>,
    text: JsonInput<Text>,
) -> Result<Response, Error> {
    let (id, thread) = path.map_err(|_| Error::Invalid)?.0;
    let (team_id, id, service) = conversation(&projects, id).await?;
    submit(service, &team_id, headers, id, Some(thread), text).await
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-authenticated",
    id = "send-record",
    method = "GET",
    path = "/slack/send-requests/{request_id}",
    summary = "查询 Slack 发送记录",
    description = "查询 Slack 发送记录"
)]
async fn send_record(
    State(projects): Service,
    headers: HeaderMap,
    scope: Params<Scope>,
    path: Segment<Uuid>,
) -> Result<Json<Value>, Error> {
    let (team_id, service) = scoped(&projects, scope).await?;
    service
        .send_record(session(&headers)?, path.map_err(|_| Error::Invalid)?.0)
        .await
        .map(|value| Json(conversation_result(value, &team_id)))
}
