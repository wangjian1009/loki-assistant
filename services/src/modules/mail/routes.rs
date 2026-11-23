use std::sync::Arc;

use axum::{
    Json,
    extract::{
        Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::Response,
};
use loki_services_macros::loki_api;
use serde::Deserialize;
use serde_json::{Value, json};

use super::MailService;
use crate::{
    http::error::{API_VERSION, ErrorSource, response},
    modules::emacs::error_response,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageQuery {
    message_id: String,
}

#[loki_api(
    module = "mail",
    mount = "Arc<MailService>",
    id = "open",
    method = "POST",
    path = "/mail/open",
    summary = "打开邮件",
    description = "在 Emacs mu4e 中打开指定 Message-ID 的邮件。"
)]
async fn open(
    State(service): State<Arc<MailService>>,
    query: Result<Query<MessageQuery>, QueryRejection>,
) -> Result<Json<Value>, Response> {
    let Query(query) = query.map_err(|_| invalid("需要提供有效的 message_id。"))?;
    if query.message_id.trim().is_empty() || query.message_id.chars().any(char::is_control) {
        return Err(invalid("需要提供有效的 message_id。"));
    }
    service
        .emacs
        .mail_action("view", json!(query.message_id))
        .await
        .map_err(error_response)?;
    Ok(opened())
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
