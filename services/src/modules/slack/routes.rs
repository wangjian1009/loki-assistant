use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, LOCATION},
    },
    middleware,
    response::{Html, IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    projects::{Projects, RegistrationStatus},
    protocol::{Error, Page},
    service::SlackService,
};

type Service = State<Arc<Projects>>;
type JsonInput<T> = Result<Json<T>, JsonRejection>;
type Params<T> = Result<Query<T>, QueryRejection>;
type Segment<T> = Result<Path<T>, PathRejection>;

pub(super) fn router(service: Arc<Projects>) -> Router {
    crate::http::mount_routes(Router::new(), "slack-local")
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
            post(team_config_changed).delete(team_config_changed),
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
        return Ok(StatusCode::NO_CONTENT);
    }
    match projects.registration_status(request.id, &team_id).await {
        RegistrationStatus::Confirmed => {
            sync_after_confirmation(projects, request);
            Ok(StatusCode::NO_CONTENT)
        }
        RegistrationStatus::Rejected => Ok(StatusCode::UNPROCESSABLE_ENTITY),
        RegistrationStatus::NeedsCredentials if request.retry => {
            if let Err(error) = projects.open_team_setup(&team_id).await {
                eprintln!("打开 Slack 工作区配置指引失败：{error}");
            }
            Ok(StatusCode::ACCEPTED)
        }
        RegistrationStatus::NeedsCredentials | RegistrationStatus::Pending => {
            Ok(StatusCode::ACCEPTED)
        }
    }
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

fn header_uuid(headers: &HeaderMap, name: &'static str) -> Result<Uuid, Error> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(Error::Invalid)
}

fn session_header(headers: &HeaderMap) -> Result<Option<Uuid>, Error> {
    const SESSION_HEADER: &str = "x-loki-slack-session";
    headers
        .contains_key(SESSION_HEADER)
        .then(|| header_uuid(headers, SESSION_HEADER))
        .transpose()
}

async fn session(headers: &HeaderMap, service: &SlackService) -> Result<Uuid, Error> {
    match session_header(headers)? {
        Some(session) => Ok(session),
        None => service.session_id().await.ok_or(Error::Session),
    }
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
    id = "team-setup",
    method = "GET",
    path = "/slack/team-setup/{team_id}",
    summary = "查看 Slack 工作区配置指引",
    description = "显示指定工作区的网页入口及会话凭据填写说明。"
)]
async fn team_setup(team: Segment<String>) -> Result<Response, Error> {
    let Path(team) = team.map_err(|_| Error::Invalid)?;
    validate_team_id(&team)?;
    Ok(slack_instruction_page(format!(
        r#"<!doctype html>
<html lang="zh-CN">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>配置 Slack 工作区</title>
<main>
  <h1>配置 Slack 工作区 {team}</h1>
  <ol>
    <li><a href="https://app.slack.com/client/{team}" target="_blank" rel="noopener noreferrer">打开 Slack 工作区</a>并登录。</li>
    <li>获取同一网页会话的 <code>xoxc-</code> 用户令牌和名为 <code>d</code> 的 Cookie 值。</li>
    <li>回到 Loki Assistant 配置界面，编辑待确认的工作区 <code>{team}</code>，按下列格式填写「Slack 网页会话凭据」并保存。</li>
  </ol>
  <pre><code>{{"token":"xoxc-…","d_cookie":"…"}}</code></pre>
</main>
</html>"#
    )))
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
    slack_instruction_page(concat!(
        "<!doctype html><html lang=zh-CN><meta charset=utf-8>",
        "<title>导入 Slack 用户令牌</title><h1>导入 Slack 用户令牌</h1>",
        "<p>打开 Loki Assistant Chrome 插件，粘贴目标工作区已有的 xoxp- 用户令牌并点击「接受」。</p>"
    ))
}

fn slack_instruction_page(html: impl Into<String>) -> Response {
    (
        [
            (axum::http::header::REFERRER_POLICY, "no-referrer"),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; base-uri 'none'; frame-ancestors 'none'",
            ),
        ],
        Html(html.into()),
    )
        .into_response()
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
    description = "提交正在导入的工作区的用户令牌，校验所属工作区后保存配置。"
)]
async fn accept_token_import(
    State(projects): Service,
    input: JsonInput<TokenImport>,
) -> Result<StatusCode, Error> {
    let Json(TokenImport { team_id, token }) = input.map_err(|_| Error::Invalid)?;
    validate_team_id(&team_id)?;
    tokio::spawn(async move { projects.accept_token(&team_id, token).await })
        .await
        .map_err(|_| Error::Unavailable)??;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
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
    service.disconnect(session_header(&headers)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
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
    mount_group = "slack-local",
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
    mount_group = "slack-local",
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
        .messages(session(&headers, &service).await?, id, None, page)
        .await
        .map(|value| Json(messages_result(value, &team_id)))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
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
        .messages(session(&headers, &service).await?, id, Some(thread), page)
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
    mount_group = "slack-local",
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
            session(&headers, &service).await?,
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
    mount_group = "slack-local",
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
            session(&headers, &service).await?,
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
            session(&headers, &service).await?,
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
    mount_group = "slack-local",
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
    mount_group = "slack-local",
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
    mount_group = "slack-local",
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
        .send_record(
            session(&headers, &service).await?,
            path.map_err(|_| Error::Invalid)?.0,
        )
        .await
        .map(|value| Json(conversation_result(value, &team_id)))
}
