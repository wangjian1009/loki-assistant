use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::Response,
};
use loki_services_macros::loki_api;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{error_response, service::EmacsService};
use crate::http::error::{API_VERSION, ErrorSource, response};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageQuery {
    message_id: String,
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "open-mail",
    method = "POST",
    path = "/emacs/mail/open",
    summary = "打开邮件",
    description = "在 Emacs mu4e 中打开指定 Message-ID 的邮件。"
)]
async fn open(
    State(service): State<Arc<EmacsService>>,
    query: Result<Query<MessageQuery>, QueryRejection>,
) -> Result<Json<Value>, Response> {
    let invalid = || {
        response(
            StatusCode::BAD_REQUEST,
            ErrorSource::Business,
            "invalid-request",
            "需要提供有效的 message_id。",
        )
    };
    let Query(query) = query.map_err(|_| invalid())?;
    if query.message_id.trim().is_empty() || query.message_id.chars().any(char::is_control) {
        return Err(invalid());
    }
    dispatch(&service, "view", json!(query.message_id)).await
}

#[loki_api(
    module = "emacs",
    mount = "Arc<EmacsService>",
    id = "open-mail-inbox",
    method = "POST",
    path = "/emacs/mail/inbox/open",
    summary = "打开收件箱",
    description = "在 Emacs mu4e 中打开配置的收件箱书签。"
)]
async fn open_inbox(State(service): State<Arc<EmacsService>>) -> Result<Json<Value>, Response> {
    dispatch(&service, "inbox", Value::Null).await
}

#[loki_api(
    module = "mail",
    mount_group = "emacs",
    mount = "Arc<EmacsService>",
    id = "open",
    method = "POST",
    path = "/mail/open",
    summary = "打开邮件",
    description = "兼容已有邮件通知，转发至 /emacs/mail/open。"
)]
async fn open_legacy(
    state: State<Arc<EmacsService>>,
    query: Result<Query<MessageQuery>, QueryRejection>,
) -> Result<Json<Value>, Response> {
    open(state, query).await
}

#[loki_api(
    module = "mail",
    mount_group = "emacs",
    mount = "Arc<EmacsService>",
    id = "open-unread",
    method = "POST",
    path = "/mail/unread/open",
    summary = "打开收件箱",
    description = "兼容旧版托盘，转发至 /emacs/mail/inbox/open。"
)]
async fn open_inbox_legacy(state: State<Arc<EmacsService>>) -> Result<Json<Value>, Response> {
    open_inbox(state).await
}

async fn dispatch(
    service: &EmacsService,
    action: &'static str,
    payload: Value,
) -> Result<Json<Value>, Response> {
    service
        .mail_action(action, payload)
        .await
        .map_err(error_response)?;
    Ok(Json(json!({"api_version": API_VERSION, "opened": true})))
}
