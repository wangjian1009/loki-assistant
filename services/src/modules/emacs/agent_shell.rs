use std::{sync::Arc, time::Duration};

use axum::{
    Extension,
    extract::{Path, State, rejection::PathRejection},
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

#[derive(Deserialize, Serialize, JsonSchema)]
struct ConversationBlock {
    id: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    text: String,
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

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "agent-shell-events",
    method = "GET",
    path = "/agent-shell/sessions/{session_id}/events",
    access = "paired",
    sse_event = ConversationEvent,
    summary = "订阅 Agent Shell 会话内容",
    description = "按状态中心公布的会话标识订阅会话内容。连接后发送当前快照，随后按版本发送内容更新。"
)]
async fn conversation_events(
    State(emacs): State<Arc<EmacsService>>,
    session_id: Result<Path<String>, PathRejection>,
    remote: Option<Extension<RemoteSse>>,
    headers: HeaderMap,
) -> Result<Response, Response> {
    let Path(session_id) = session_id.map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            ErrorSource::Services,
            "invalid-request",
            "Agent Shell 会话的 buffer 名称编码无效。",
        )
    })?;
    if remote.is_none() && !sse::accepts_event_stream(&headers) {
        return Err(error_response(
            StatusCode::NOT_ACCEPTABLE,
            ErrorSource::Services,
            "agent-shell-stream-required",
            "请通过事件流订阅 Agent Shell 会话。",
        ));
    }

    let initial = read_conversation(&emacs, &session_id, None)
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
        Some((emacs, session_id, initial.revision)),
        |state| async move {
            let (emacs, session_id, revision) = state?;
            let (next_revision, change) = loop {
                match read_conversation(&emacs, &session_id, Some(revision)).await {
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
                continued.then_some((emacs, session_id, next_revision)),
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
) -> Result<Option<ConversationRead>, ReadError> {
    let cursor = revision.map_or_else(|| "nil".to_owned(), |revision| revision.to_string());
    let name = STANDARD.encode(session_id);
    let script = format!(
        "(when (require 'loki-assistant-agent-shell nil t) \
           (when (fboundp 'loki-assistant-agent-shell-stream-read) \
             (let* ((name (decode-coding-string (base64-decode-string \"{name}\") 'utf-8)) \
                    (reply (loki-assistant-agent-shell-stream-read name {cursor}))) \
               (when (stringp reply) \
                 (base64-encode-string (encode-coding-string reply 'utf-8) t)))))"
    );
    let reply = emacs
        .eval_running(&script)
        .await
        .map_err(|error| match error {
            EmacsError::Timeout => ReadError::Timeout,
            EmacsError::InvalidResponse(_) => ReadError::InvalidResponse,
            _ => ReadError::Unavailable,
        })?
        .filter(|reply| reply != "nil")
        .ok_or(ReadError::Unavailable)?;
    let reply: String = serde_json::from_str(&reply).map_err(|_| ReadError::InvalidResponse)?;
    let reply = STANDARD
        .decode(reply)
        .map_err(|_| ReadError::InvalidResponse)?;
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
