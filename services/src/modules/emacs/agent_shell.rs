use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use loki_services_macros::loki_api;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::http::{
    error::{ErrorSource, response as error_response},
    sse::{self, RemoteSse, SseEvent},
};

use super::{
    protocol::API_VERSION,
    service::{EmacsError, EmacsService},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_IMAGE_SOURCE_BYTES: usize = 4096;
const MAX_IMAGE_BYTES: usize = 1024 * 1024;

#[derive(Deserialize, Serialize, JsonSchema)]
struct ConversationBlock {
    id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    collapsed: Option<bool>,
}

#[derive(Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConversationChange {
    Snapshot { blocks: Vec<ConversationBlock> },
    Update { blocks: Vec<ConversationBlock> },
    Closed,
    Error { message: &'static str },
}

#[derive(Serialize, JsonSchema)]
struct ConversationEvent {
    api_version: &'static str,
    session_id: String,
    revision: u64,
    #[serde(flatten)]
    change: ConversationChange,
}

impl SseEvent for ConversationEvent {
    const EVENT: &'static str = "agent-shell";
}

struct ConversationRead {
    revision: u64,
    change: ConversationChange,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationQuery {
    recent: Option<u8>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EmacsConversationRead {
    Snapshot {
        revision: u64,
        blocks: Vec<ConversationBlock>,
    },
    Update {
        revision: u64,
        blocks: Vec<ConversationBlock>,
    },
    Closed {
        revision: u64,
    },
    Missing,
}

enum ReadError {
    Unavailable,
    InvalidResponse,
    Timeout,
    Missing,
}

impl ReadError {
    fn details(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "emacs-unavailable",
                "无法读取 Emacs 上的 Agent Shell 会话。",
            ),
            Self::InvalidResponse => (
                StatusCode::BAD_GATEWAY,
                "invalid-emacs-response",
                "Emacs 返回的 Agent Shell 会话数据无效。",
            ),
            Self::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "emacs-timeout",
                "读取 Agent Shell 会话超时。",
            ),
            Self::Missing => (
                StatusCode::NOT_FOUND,
                "agent-shell-session-not-found",
                "找不到 Agent Shell 会话。",
            ),
        }
    }

    fn into_response(self) -> Response {
        let (status, code, message) = self.details();
        let source = match self {
            Self::Missing => ErrorSource::Business,
            _ => ErrorSource::Emacs,
        };
        error_response(status, source, code, message)
    }
}

fn conversation_session_id(
    session_id: Result<Path<String>, PathRejection>,
) -> Result<String, Response> {
    let Path(session_id) = session_id.map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            ErrorSource::Services,
            "invalid-request",
            "Agent Shell 会话的 buffer 名称编码无效。",
        )
    })?;
    Ok(session_id)
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "agent-shell-events",
    method = "GET",
    path = "/agent-shell/sessions/{session_id}/events",
    access = "paired",
    sse_event = ConversationEvent,
    summary = "订阅 Agent Shell 会话内容",
    description = "按状态中心公布的会话标识订阅会话内容。recent=10 从最近十条开始，省略时读取完整历史。快照之后按版本发送更新。"
)]
async fn conversation_events(
    State(emacs): State<Arc<EmacsService>>,
    session_id: Result<Path<String>, PathRejection>,
    query: Result<Query<ConversationQuery>, QueryRejection>,
    remote: Option<Extension<RemoteSse>>,
    headers: HeaderMap,
) -> Result<Response, Response> {
    let session_id = conversation_session_id(session_id)?;
    if remote.is_none() && !sse::accepts_event_stream(&headers) {
        return Err(error_response(
            StatusCode::NOT_ACCEPTABLE,
            ErrorSource::Services,
            "agent-shell-stream-required",
            "请通过事件流订阅 Agent Shell 会话。",
        ));
    }
    let Query(query) = query
        .ok()
        .filter(|Query(query)| matches!(query.recent, None | Some(10)))
        .ok_or_else(|| conversation_input_error("recent 只能取 10。"))?;
    let recent = query.recent.is_some();

    let initial = read_conversation(&emacs, &session_id, None, recent)
        .await
        .map_err(ReadError::into_response)?
        .ok_or_else(|| ReadError::InvalidResponse.into_response())?;
    if !matches!(initial.change, ConversationChange::Snapshot { .. }) {
        return Err(ReadError::InvalidResponse.into_response());
    }

    let initial_event = ConversationEvent {
        api_version: API_VERSION,
        session_id: session_id.clone(),
        revision: initial.revision,
        change: initial.change,
    };
    let updates = stream::unfold(
        Some((emacs, session_id, initial.revision, recent)),
        |state| async move {
            let (emacs, session_id, revision, recent) = state?;
            let (next_revision, change) = loop {
                match read_conversation(&emacs, &session_id, Some(revision), recent).await {
                    Ok(None) => tokio::time::sleep(POLL_INTERVAL).await,
                    Ok(Some(update)) => break (update.revision, update.change),
                    Err(ReadError::Missing) => break (revision, ConversationChange::Closed),
                    Err(error) => {
                        break (
                            revision,
                            ConversationChange::Error {
                                message: error.details().2,
                            },
                        );
                    }
                }
            };
            let continued = matches!(
                change,
                ConversationChange::Snapshot { .. } | ConversationChange::Update { .. }
            );
            let event = ConversationEvent {
                api_version: API_VERSION,
                session_id: session_id.clone(),
                revision: next_revision,
                change,
            };
            Some((
                event,
                continued.then_some((emacs, session_id, next_revision, recent)),
            ))
        },
    );
    let events = stream::iter([initial_event]).chain(updates);
    Ok((
        [(CACHE_CONTROL, "no-store")],
        sse::response(events, remote.map(|Extension(remote)| remote)),
    )
        .into_response())
}

async fn read_conversation(
    emacs: &EmacsService,
    session_id: &str,
    revision: Option<u64>,
    recent: bool,
) -> Result<Option<ConversationRead>, ReadError> {
    let cursor = revision.map_or_else(|| "nil".to_owned(), |revision| revision.to_string());
    let limit = if recent { " 10" } else { "" };
    let name = STANDARD.encode(session_id);
    let script = format!(
        "(when (require 'loki-assistant-agent-shell nil t) \
           (when (fboundp 'loki-assistant-agent-shell-stream-read) \
             (let* ((name (decode-coding-string (base64-decode-string \"{name}\") 'utf-8)) \
                    (reply (loki-assistant-agent-shell-stream-read name {cursor}{limit}))) \
               (when (stringp reply) \
                 (base64-encode-string (encode-coding-string reply 'utf-8) t)))))"
    );
    let reply = conversation_gateway(emacs, &script)
        .await?
        .ok_or(ReadError::Unavailable)?;
    let read: Option<EmacsConversationRead> =
        serde_json::from_slice(&reply).map_err(|_| ReadError::InvalidResponse)?;
    let Some(read) = read else { return Ok(None) };
    let (revision, change) = match read {
        EmacsConversationRead::Snapshot { revision, blocks } => {
            (revision, ConversationChange::Snapshot { blocks })
        }
        EmacsConversationRead::Update {
            revision: next,
            blocks,
        } if revision.is_some_and(|current| next > current) => {
            (next, ConversationChange::Update { blocks })
        }
        EmacsConversationRead::Update { .. } => return Err(ReadError::InvalidResponse),
        EmacsConversationRead::Closed { revision: next } if revision.is_some() => {
            (next, ConversationChange::Closed)
        }
        EmacsConversationRead::Closed { .. } | EmacsConversationRead::Missing => {
            return Err(ReadError::Missing);
        }
    };
    Ok(Some(ConversationRead { revision, change }))
}

async fn conversation_gateway(
    emacs: &EmacsService,
    script: &str,
) -> Result<Option<Vec<u8>>, ReadError> {
    let reply = emacs
        .eval_running(script)
        .await
        .map_err(|error| match error {
            EmacsError::Timeout => ReadError::Timeout,
            EmacsError::InvalidResponse(_) => ReadError::InvalidResponse,
            _ => ReadError::Unavailable,
        })?
        .ok_or(ReadError::Unavailable)?;
    if reply == "nil" {
        return Ok(None);
    }
    let reply: String = serde_json::from_str(&reply).map_err(|_| ReadError::InvalidResponse)?;
    STANDARD
        .decode(reply)
        .map(Some)
        .map_err(|_| ReadError::InvalidResponse)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageRequest {
    text: String,
}

#[derive(Serialize)]
struct MessageResponse {
    api_version: &'static str,
    accepted: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MessageOutcome {
    Accepted,
    Missing,
    Unavailable,
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "agent-shell-submit-message",
    method = "POST",
    path = "/agent-shell/sessions/{session_id}/messages",
    access = "paired",
    summary = "向 Agent Shell 会话发送消息",
    description = "以文本向状态中心公布的 Agent Shell 会话提交消息，最多 16 KiB UTF-8。Emacs 按会话状态直接提交、引导正在运行的轮次或排队。"
)]
async fn submit_message(
    State(emacs): State<Arc<EmacsService>>,
    session_id: Result<Path<String>, PathRejection>,
    body: Result<Json<MessageRequest>, JsonRejection>,
) -> Result<Response, Response> {
    let session_id = conversation_session_id(session_id)?;
    let Json(body) = body
        .ok()
        .filter(|Json(body)| {
            body.text.len() <= MAX_MESSAGE_BYTES
                && !body.text.contains('\0')
                && !body.text.trim().is_empty()
        })
        .ok_or_else(|| conversation_input_error("消息须为非空文本且最多 16 KiB UTF-8。"))?;
    let outcome = conversation_gateway(
        &emacs,
        &conversation_call(
            &session_id,
            &body.text,
            "loki-assistant-agent-shell-send-message",
        ),
    )
    .await
    .and_then(|reply| reply.ok_or(ReadError::Unavailable))
    .and_then(|reply| {
        serde_json::from_slice::<MessageOutcome>(&reply).map_err(|_| ReadError::InvalidResponse)
    })
    .map_err(ReadError::into_response)?;

    match outcome {
        MessageOutcome::Accepted => Ok((
            StatusCode::ACCEPTED,
            [(CACHE_CONTROL, "no-store")],
            Json(MessageResponse {
                api_version: API_VERSION,
                accepted: true,
            }),
        )
            .into_response()),
        MessageOutcome::Missing => Err(ReadError::Missing.into_response()),
        MessageOutcome::Unavailable => Err(ReadError::Unavailable.into_response()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageQuery {
    source: String,
}

#[derive(Deserialize, Serialize)]
struct ConversationImage {
    mime: String,
    data: String,
}

#[derive(Serialize)]
struct ImageResponse {
    api_version: &'static str,
    #[serde(flatten)]
    image: ConversationImage,
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "agent-shell-read-image",
    method = "GET",
    path = "/agent-shell/sessions/{session_id}/image",
    access = "paired",
    summary = "读取 Agent Shell 会话图片",
    description = "由 Emacs 读取会话中的图片引用，返回 MIME 类型与 Base64 数据。"
)]
async fn read_image(
    State(emacs): State<Arc<EmacsService>>,
    session_id: Result<Path<String>, PathRejection>,
    query: Result<Query<ImageQuery>, QueryRejection>,
) -> Result<Response, Response> {
    let session_id = conversation_session_id(session_id)?;
    let Query(query) = query
        .ok()
        .filter(|Query(query)| {
            !query.source.trim().is_empty()
                && query.source.len() <= MAX_IMAGE_SOURCE_BYTES
                && !query.source.chars().any(char::is_control)
        })
        .ok_or_else(|| conversation_input_error("图片来源无效。"))?;
    let reply = conversation_gateway(
        &emacs,
        &conversation_call(
            &session_id,
            &query.source,
            "loki-assistant-agent-shell-read-image",
        ),
    )
    .await
    .and_then(|reply| reply.ok_or(ReadError::Unavailable))
    .and_then(|reply| {
        serde_json::from_slice::<Option<ConversationImage>>(&reply)
            .map_err(|_| ReadError::InvalidResponse)
    })
    .map_err(ReadError::into_response)?;
    let image = reply.ok_or_else(|| {
        error_response(
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "agent-shell-image-not-found",
            "会话中找不到可读取的图片。",
        )
    })?;
    if !valid_image(&image) {
        return Err(ReadError::InvalidResponse.into_response());
    }
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(ImageResponse {
            api_version: API_VERSION,
            image,
        }),
    )
        .into_response())
}

fn valid_image(image: &ConversationImage) -> bool {
    let format = match image.mime.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        _ => return false,
    };
    image.data.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4
        && STANDARD.decode(&image.data).is_ok_and(|bytes| {
            bytes.len() <= MAX_IMAGE_BYTES
                && image::guess_format(&bytes).is_ok_and(|actual| actual == format)
        })
}

fn conversation_input_error(message: &'static str) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        ErrorSource::Services,
        "invalid-request",
        message,
    )
}

fn conversation_call(session_id: &str, argument: &str, function: &'static str) -> String {
    let name = STANDARD.encode(session_id);
    let argument = STANDARD.encode(argument);
    format!(
        "(when (require 'loki-assistant-agent-shell nil t) \
           (when (fboundp '{function}) \
             (let* ((name (decode-coding-string (base64-decode-string \"{name}\") 'utf-8)) \
                    (argument (decode-coding-string (base64-decode-string \"{argument}\") 'utf-8)) \
                    (reply ({function} name argument))) \
               (base64-encode-string (encode-coding-string (or reply \"null\") 'utf-8) t))))"
    )
}
