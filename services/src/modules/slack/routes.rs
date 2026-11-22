use std::{collections::BTreeSet, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, Query, State,
        rejection::{BytesRejection, JsonRejection, PathRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode, Uri,
        header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION, X_CONTENT_TYPE_OPTIONS},
    },
    middleware,
    response::{Html, IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::modules::config::service::{NotificationConfirmedFields, NotificationValidationError};

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
) -> Result<Response, Error> {
    validate_team_id(&team_id)?;
    if request.deleted || request.password_entry_removed {
        sync_after_confirmation(projects, request, None);
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    match projects.registration_status(request.id, &team_id).await {
        RegistrationStatus::Confirmed(identity) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            let reload_team = identity.is_some().then_some(team_id);
            if let Some(identity) = identity {
                let fields = [
                    ("team_name".into(), json!(identity.team_name)),
                    ("user_id".into(), json!(identity.user_id)),
                ]
                .into_iter()
                .collect();
                response
                    .extensions_mut()
                    .insert(NotificationConfirmedFields(fields));
            }
            sync_after_confirmation(projects, request, reload_team);
            Ok(response)
        }
        RegistrationStatus::Invalid(error) => {
            let error = error.object();
            let mut response = StatusCode::UNPROCESSABLE_ENTITY.into_response();
            response
                .extensions_mut()
                .insert(NotificationValidationError {
                    code: error["code"]
                        .as_str()
                        .unwrap_or("slack-validation-failed")
                        .into(),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("Slack 凭据校验失败。")
                        .into(),
                    clear_password_fields: super::projects::SlackTeam::PASSWORD_FIELDS
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                });
            Ok(response)
        }
        RegistrationStatus::NeedsCredentials => {
            if let Err(error) = projects.open_team(&team_id).await {
                eprintln!("打开 Slack 工作区配置页失败：{error}");
            }
            Ok(StatusCode::ACCEPTED.into_response())
        }
        RegistrationStatus::Pending => Ok(StatusCode::ACCEPTED.into_response()),
    }
}

fn sync_after_confirmation(
    projects: Arc<Projects>,
    request: crate::modules::config::service::NotificationRequest,
    reload_team: Option<String>,
) {
    tokio::spawn(async move {
        if request.confirmed().await
            && let Err(error) = projects.sync_projects(reload_team.as_deref()).await
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

fn split_id(id: &str) -> Result<(&str, &str), Error> {
    let (team_id, id) = id.split_once('.').ok_or(Error::Invalid)?;
    validate_team_id(team_id)?;
    if !super::protocol::valid_id(id) {
        return Err(Error::Invalid);
    }
    Ok((team_id, id))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "watches",
    method = "GET",
    path = "/slack/watches",
    summary = "列出所有关注的 Slack 主题聊天",
    description = "一次返回本机关注的全部主题聊天。"
)]
async fn watches(State(projects): Service, uri: Uri) -> Result<Json<Value>, Error> {
    if uri.query().is_some_and(|query| !query.is_empty()) {
        return Err(Error::Invalid);
    }
    let mut watches = projects.watches().await?;
    watches.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    let items: Vec<_> = watches
        .into_iter()
        .map(|watch| json!({"conversation_id": watch.id}))
        .collect();
    Ok(Json(json!({"api_version": "1", "items": items})))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "get-watch",
    method = "GET",
    path = "/slack/conversations/{id}/watch",
    summary = "查看 Slack 主题聊天的关注状态",
    description = "从本机配置读取指定会话的关注状态。"
)]
async fn get_watch(State(projects): Service, id: Segment<String>) -> Result<Json<Value>, Error> {
    let Path(id) = id.map_err(|_| Error::Invalid)?;
    split_id(&id)?;
    let watching = projects.watches().await?.iter().any(|watch| watch.id == id);
    Ok(Json(
        json!({"api_version": "1", "conversation_id": id, "watching": watching}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchInput {
    watching: bool,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "set-watch",
    method = "PUT",
    path = "/slack/conversations/{id}/watch",
    summary = "设置 Slack 主题聊天的关注状态",
    description = "在本机配置中开启或取消指定主题聊天的关注。"
)]
async fn set_watch(
    State(projects): Service,
    id: Segment<String>,
    input: JsonInput<WatchInput>,
) -> Result<Json<Value>, Error> {
    let Path(id) = id.map_err(|_| Error::Invalid)?;
    let Json(WatchInput { watching }) = input.map_err(|_| Error::Invalid)?;
    // 已收到的设置请求独立完成提交与通知队列清理。
    tokio::spawn(async move {
        let (team, conversation) = split_id(&id)?;
        projects
            .set_watch(id.clone(), team, conversation, watching)
            .await?;
        Ok(Json(
            json!({"api_version": "1", "conversation_id": id, "watching": watching}),
        ))
    })
    .await
    .map_err(|_| Error::WatchStorage)?
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
    let (team_id, id) = split_id(&composite_id)?;
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

pub(super) fn messages_result(mut value: Value, team_id: &str) -> Value {
    fn qualify_author(message: &mut Value, team_id: &str) {
        if let Some(id) = message["author"]["id"].as_str() {
            message["author"]["id"] = json!(format!("{team_id}.{id}"));
        }
    }

    for key in ["items", "thread_items"] {
        for message in value
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
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
    id = "subscribe-messages",
    method = "PUT",
    path = "/slack/conversations/{id}/subscriptions/{subscription_id}",
    summary = "订阅 Slack 会话消息",
    description = "登记 Emacs 聊天 buffer，向现有 Emacs server 推送快照和消息变更。"
)]
async fn subscribe_messages(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, Uuid)>,
    body: Result<Bytes, BytesRejection>,
) -> Result<(StatusCode, Json<Value>), Error> {
    let (id, subscription) = path.map_err(|_| Error::Invalid)?.0;
    let session = session_header(&headers)?.ok_or(Error::Invalid)?;
    if !body.map_err(|_| Error::Invalid)?.is_empty() {
        return Err(Error::Invalid);
    }
    let (_, room, service) = conversation(&projects, id.clone()).await?;
    let created = service.subscribe_messages(session, room, subscription).await?;
    Ok((
        if created { StatusCode::CREATED } else { StatusCode::OK },
        Json(json!({"api_version":"1", "subscription_id":subscription,
            "conversation_id":id, "session_id":session})),
    ))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "unsubscribe-messages",
    method = "DELETE",
    path = "/slack/conversations/{id}/subscriptions/{subscription_id}",
    summary = "取消 Slack 会话消息订阅",
    description = "按聊天和订阅标识停止 Emacs 消息投递。"
)]
async fn unsubscribe_messages(
    State(projects): Service,
    path: Segment<(String, Uuid)>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, Error> {
    let (id, subscription) = path.map_err(|_| Error::Invalid)?.0;
    let (team, room) = split_id(&id)?;
    if !body.map_err(|_| Error::Invalid)?.is_empty() {
        return Err(Error::Invalid);
    }
    match projects.service(team).await {
        Ok(service) => service.unsubscribe_messages(room.to_owned(), subscription).await,
        Err(Error::Project) => {},
        Err(error) => return Err(error),
    }
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "team-setup",
    method = "GET",
    path = "/slack/team-setup/{team_id}",
    summary = "打开 Slack 工作区配置",
    description = "填写网页会话凭据并提交到配置模块。"
)]
async fn team_setup(team: Segment<String>) -> Result<Response, Error> {
    let Path(team) = team.map_err(|_| Error::Invalid)?;
    validate_team_id(&team)?;
    let nonce = Uuid::new_v4().simple().to_string();
    let page = include_str!("team-setup.html")
        .replace("{{team_id}}", &team)
        .replace("{{nonce}}", &nonce);
    Ok((
        [
            (axum::http::header::REFERRER_POLICY, "no-referrer".to_owned()),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                format!("default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'"),
            ),
        ],
        Html(page),
    ).into_response())
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
    summary = "查询 Slack 用户",
    description = "使用重复的 id 参数筛选用户，省略 id 时分页读取用户目录。"
)]
async fn users(State(projects): Service, uri: Uri) -> Result<Json<Value>, Error> {
    let query = uri.query().unwrap_or_default();
    let mut ids = Vec::new();
    let mut has_other_params = false;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if key == "id" {
            if ids.len() == 100 {
                return Err(Error::Invalid);
            }
            ids.push(value);
        } else {
            has_other_params = true;
        }
    }
    if ids.is_empty() {
        let Query(page) = Query::<MessagePage>::try_from_uri(&uri).map_err(|_| Error::Invalid)?;
        return projects.users(page.into()).await.map(Json);
    }
    if has_other_params {
        return Err(Error::Invalid);
    }
    let mut seen = BTreeSet::new();
    let users = ids
        .iter()
        .map(|id| id.as_ref())
        .filter(|id| seen.insert(*id))
        .map(|id| split_id(id).map(|(team, user)| (team.to_owned(), user.to_owned())))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(projects.lookup_users(users).await))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PickerQuery {}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "conversation-picker",
    method = "GET",
    path = "/slack/conversations/picker",
    summary = "读取 Slack 会话候选",
    description = "一次读取全部工作区的会话候选内存快照。"
)]
async fn picker(
    State(projects): Service,
    query: Params<PickerQuery>,
) -> Result<Json<Value>, Error> {
    query.map_err(|_| Error::Invalid)?;
    Ok(Json(projects.picker().await))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenQuery {
    thread_ts: Option<String>,
    message_ts: Option<String>,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "open-conversation",
    method = "POST",
    path = "/slack/conversations/{id}/open",
    summary = "打开 Slack 会话",
    description = "在 Emacs 中打开会话，可定位线程或消息。"
)]
async fn open_conversation(
    State(projects): Service,
    headers: HeaderMap,
    id: Segment<String>,
    query: Params<OpenQuery>,
) -> Result<Json<Value>, Response> {
    let Query(query) = query.map_err(|_| Error::Invalid.into_response())?;
    if [query.thread_ts.as_deref(), query.message_ts.as_deref()]
        .into_iter()
        .flatten()
        .any(|ts| super::protocol::timestamp(ts).is_none())
    {
        return Err(Error::Invalid.into_response());
    }
    let Path(id) = id.map_err(|_| Error::Invalid.into_response())?;
    let (_, raw_id, service) = conversation(&projects, id.clone())
        .await
        .map_err(IntoResponse::into_response)?;
    let session = session(&headers, &service)
        .await
        .map_err(IntoResponse::into_response)?;
    let target = service
        .open_target(
            session,
            &raw_id,
            query.thread_ts.as_deref(),
            query.message_ts.as_deref(),
        )
        .await
        .map_err(IntoResponse::into_response)?;
    let target = projects
        .emacs
        .open_slack_conversation(&target)
        .await
        .map_err(crate::modules::emacs::error_response)?;
    Ok(Json(
        json!({"api_version":"1", "conversation_id":id, "target":target}),
    ))
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
    let session = session(&headers, &service).await?;
    let value = service.messages(session, id.clone(), None, page).await?;
    let roots = super::subscriptions::thread_roots(&value);
    if !roots.is_empty() && service.has_message_subscribers(&id).await {
        tokio::spawn(async move { service.refresh_threads(session, &id, roots).await });
    }
    Ok(Json(messages_result(value, &team_id)))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentQuery {
    #[serde(default)]
    local_path: bool,
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "user-avatar",
    method = "GET",
    path = "/slack/users/{id}/avatar",
    summary = "读取 Slack 用户头像路径",
    description = "按需缓存头像并返回本机文件路径。"
)]
async fn user_avatar(
    State(projects): Service,
    headers: HeaderMap,
    id: Segment<String>,
) -> Result<Json<Value>, Error> {
    let Path(id) = id.map_err(|_| Error::Invalid)?;
    let (team, user) = split_id(&id)?;
    let service = projects.service(team).await?;
    let path = service
        .avatar(session(&headers, &service).await?, user)
        .await?;
    Ok(Json(json!({"cache_path":path})))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "attachment-original",
    method = "GET",
    path = "/slack/conversations/{id}/messages/{ts}/attachments/{index}/original",
    summary = "读取 Slack 附件原件路径",
    description = "按需缓存附件原件并返回本机文件路径。"
)]
async fn attachment_original(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, String, usize)>,
) -> Result<Json<Value>, Error> {
    let Path((id, ts, index)) = path.map_err(|_| Error::Invalid)?;
    if super::protocol::timestamp(&ts).is_none() {
        return Err(Error::Invalid);
    }
    let (_, id, service) = conversation(&projects, id).await?;
    let (_, _, path) = service
        .attachment(
            session(&headers, &service).await?,
            &id,
            &ts,
            index,
            false,
            true,
        )
        .await?;
    Ok(Json(json!({"cache_path":path.ok_or(Error::Unavailable)?})))
}

#[loki_api(
    module = "slack",
    mount = "Arc<Projects>",
    mount_group = "slack-local",
    id = "attachment-preview",
    method = "GET",
    path = "/slack/conversations/{id}/messages/{ts}/attachments/{index}/preview",
    summary = "读取 Slack 图片附件预览",
    description = "从当前工作区会话的消息读取指定图片附件。"
)]
async fn attachment_preview(
    State(projects): Service,
    headers: HeaderMap,
    path: Segment<(String, String, usize)>,
    query: Params<AttachmentQuery>,
) -> Result<Response, Error> {
    let Query(query) = query.map_err(|_| Error::Invalid)?;
    let Path((id, ts, index)) = path.map_err(|_| Error::Invalid)?;
    if super::protocol::timestamp(&ts).is_none() {
        return Err(Error::Invalid);
    }
    let (_, id, service) = conversation(&projects, id).await?;
    let (mime, image, path) = service
        .attachment(
            session(&headers, &service).await?,
            &id,
            &ts,
            index,
            true,
            query.local_path,
        )
        .await?;
    if query.local_path {
        return Ok(Json(json!({"cache_path":path.ok_or(Error::Unavailable)?})).into_response());
    }
    Ok((
        [
            (CONTENT_TYPE, mime.as_str()),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        image,
    )
        .into_response())
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
