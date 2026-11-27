use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, StatusCode,
        header::{ACCEPT, CACHE_CONTROL, VARY},
    },
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, stream};
use loki_services_macros::loki_api;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{MailService, progress::Snapshot, sync::IndexUpdate};
use crate::{
    http::{
        error::{API_VERSION, ErrorSource, response},
        sse::{self, RemoteSse, SseEvent},
    },
    modules::emacs::error_response,
};

#[derive(Serialize, JsonSchema)]
struct SyncEvent {
    api_version: &'static str,
    sync: Snapshot,
}

#[derive(Serialize)]
struct SyncAccepted {
    api_version: &'static str,
    id: Uuid,
}

impl SyncEvent {
    fn new(sync: Snapshot) -> Self {
        Self {
            api_version: API_VERSION,
            sync,
        }
    }
}

impl SseEvent for SyncEvent {
    const EVENT: &'static str = "sync";
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "index-result",
    method = "POST",
    path = "/mail/index/result/{id}",
    summary = "接收 Emacs 邮件索引进度和结果",
    description = "接收已发起的索引任务的统计更新和最终结果。"
)]
async fn index_result(
    State(service): State<Arc<MailService>>,
    Path(id): Path<String>,
    result: Result<Json<IndexUpdate>, JsonRejection>,
) -> Result<Json<Value>, Response> {
    let id = Uuid::parse_str(&id).map_err(|_| invalid("邮件索引请求 ID 无效。"))?;
    let Json(result) = result.map_err(|_| invalid("邮件索引进度或结果无效。"))?;
    if !service.sync_progress.receive_index_update(id, result).await {
        return Err(response(
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "mail-index-request-not-found",
            "找不到待接收结果的邮件索引请求。",
        ));
    }
    Ok(Json(json!({"api_version": API_VERSION})))
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "sync",
    method = "POST",
    path = "/mail/sync",
    access = "paired",
    sse_event = SyncEvent,
    summary = "立即同步邮件",
    description = "开始或加入当前邮件同步任务。可请求 SSE 进度，默认返回任务 ID。"
)]
async fn start_sync(
    State(service): State<Arc<MailService>>,
    remote: Option<Extension<RemoteSse>>,
    headers: HeaderMap,
) -> Response {
    let (initial, updates) = service.sync_progress.request().await;
    let body = if accepts_progress(&headers) {
        let updates = stream::unfold(updates, |mut updates| async {
            updates.changed().await.ok()?;
            let sync = updates.borrow_and_update().clone();
            Some((sync, updates))
        });
        let events = stream::iter([initial]).chain(updates).map(SyncEvent::new);
        sse::response(events, remote.map(|Extension(remote)| remote))
    } else {
        Json(SyncAccepted {
            api_version: API_VERSION,
            id: initial.id,
        })
        .into_response()
    };
    (
        StatusCode::ACCEPTED,
        [(CACHE_CONTROL, "no-store"), (VARY, "Accept")],
        body,
    )
        .into_response()
}

fn accepts_progress(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value.split(',').any(|range| {
                let mut parts = range.split(';');
                let media_type = parts.next().unwrap_or_default().trim();
                media_type.eq_ignore_ascii_case(sse::MEDIA_TYPE)
                    && parts
                        .filter_map(|param| param.trim().split_once('='))
                        .find(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
                        .is_none_or(|(_, quality)| {
                            quality
                                .trim()
                                .parse::<f32>()
                                .is_ok_and(|value| value > 0.0 && value <= 1.0)
                        })
            })
        })
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "sync-progress",
    method = "GET",
    path = "/mail/sync/{id}",
    summary = "查询邮件同步进度",
    description = "返回同步阶段、最近的 mu 索引消息和失败原因。"
)]
async fn sync_progress(
    State(service): State<Arc<MailService>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Uuid::parse_str(&id) else {
        return invalid("邮件同步任务 ID 无效。");
    };
    let Some(sync) = service.sync_progress.snapshot(id).await else {
        return response(
            StatusCode::NOT_FOUND,
            ErrorSource::Business,
            "mail-sync-not-found",
            "找不到该邮件同步任务。",
        );
    };
    ([(CACHE_CONTROL, "no-store")], Json(SyncEvent::new(sync))).into_response()
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "refresh-status",
    method = "POST",
    path = "/mail/status/refresh",
    summary = "请求刷新邮件状态",
    description = "Emacs 邮件操作完成后，安排从当前 mu4e 数据库重查未读数。"
)]
async fn refresh_status(State(service): State<Arc<MailService>>) -> (StatusCode, Json<Value>) {
    service.refresh_requested.notify_one();
    (
        StatusCode::ACCEPTED,
        Json(json!({"api_version": API_VERSION})),
    )
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "open-config",
    method = "POST",
    path = "/mail/config/open",
    summary = "打开邮件同步配置",
    description = "在 Emacs 中打开当前生效的 mbsync 配置文件。"
)]
async fn open_config(State(service): State<Arc<MailService>>) -> Result<Json<Value>, Response> {
    let path = service.config_path().await.map_err(|error| {
        response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorSource::Services,
            "mail-config-error",
            error.to_string(),
        )
    })?;
    service
        .emacs
        .mail_action("config", json!(path))
        .await
        .map_err(error_response)?;
    Ok(opened())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComposeRequest {
    url: String,
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "compose",
    method = "POST",
    path = "/mail/compose",
    summary = "撰写新邮件",
    description = "将系统 mailto URL 转为 Emacs mu4e 草稿，由用户确认发送。"
)]
async fn compose(
    State(service): State<Arc<MailService>>,
    request: Result<Json<ComposeRequest>, JsonRejection>,
) -> Result<Json<Value>, Response> {
    let Json(request) = request.map_err(|_| invalid("需要提供 JSON 字段 url。"))?;
    let draft = parse_mailto(&request.url).map_err(invalid)?;
    service
        .emacs
        .mail_action("compose", draft)
        .await
        .map_err(error_response)?;
    Ok(opened())
}

fn opened() -> Json<Value> {
    Json(json!({"api_version": API_VERSION, "opened": true}))
}

fn invalid(message: &'static str) -> Response {
    response(
        StatusCode::BAD_REQUEST,
        ErrorSource::Business,
        "invalid-request",
        message,
    )
}

fn parse_mailto(url: &str) -> Result<Value, &'static str> {
    const INVALID: &str = "需要提供有效的 mailto URL。";
    let (scheme, value) = url.split_once(':').ok_or(INVALID)?;
    if !scheme.eq_ignore_ascii_case("mailto")
        || value.starts_with("//")
        || value.contains('#')
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(INVALID);
    }

    let (address, query) = value.split_once('?').unwrap_or((value, ""));
    let mut draft = json!({"to": decode(address, false)?});
    for field in query.split('&').filter(|field| !field.is_empty()) {
        let (key, value) = field.split_once('=').ok_or(INVALID)?;
        let key = decode(key, false)?.to_ascii_lowercase();
        let body = key == "body";
        if !matches!(key.as_str(), "to" | "cc" | "bcc" | "subject" | "body") {
            continue;
        }
        let value = decode(value, body)?;
        match (key.as_str(), draft.get(&key).and_then(Value::as_str)) {
            ("to" | "cc" | "bcc", Some(previous)) if !previous.is_empty() => {
                if !value.is_empty() {
                    draft[&key] = json!(format!("{previous}, {value}"));
                }
            }
            ("subject" | "body", Some(_)) => return Err("mailto URL 的主题和正文只能各提供一次。"),
            _ => draft[&key] = json!(value),
        }
    }
    Ok(draft)
}

fn decode(value: &str, body: bool) -> Result<String, &'static str> {
    const INVALID: &str = "mailto URL 含有无效编码或控制字符。";
    for (index, _) in value.match_indices('%') {
        if !value
            .as_bytes()
            .get(index + 1..index + 3)
            .is_some_and(|bytes| bytes.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(INVALID);
        }
    }
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map_err(|_| INVALID)?;
    if decoded
        .chars()
        .any(|c| c.is_control() && !(body && matches!(c, '\r' | '\n' | '\t')))
    {
        return Err(INVALID);
    }
    Ok(if body {
        decoded.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        decoded.into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailto_preserves_plus_and_decodes_supported_fields() {
        assert_eq!(
            parse_mailto("MAILTO:a+tag@example.org?to=b@example.org&cc=c@example.org&subject=%E9%82%AE%E4%BB%B6&body=x%0D%0Ay&from=ignored").unwrap(),
            json!({"to":"a+tag@example.org, b@example.org", "cc":"c@example.org", "subject":"邮件", "body":"x\ny"}),
        );
        assert_eq!(parse_mailto("mailto:").unwrap(), json!({"to": ""}));
    }

    #[test]
    fn mailto_rejects_header_injection_and_malformed_input() {
        for url in [
            "https://example.org",
            "mailto://example.org",
            "mailto:a?subject=one%0abcc:two",
            "mailto:a%FF",
            "mailto:a?body=%0Z",
            "mailto:a?subject=x&subject=y",
        ] {
            assert!(parse_mailto(url).is_err(), "{url}");
        }
    }
}
