use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use loki_services_macros::loki_config;
use serde_json::{Value, json};
use url::Url;

use crate::modules::config::{
    model::{
        CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigItemOptions,
        ConfigModuleMetadata, ListMetadata, StructureField, StructureMetadata,
    },
    registry::ConfigItemRegistration,
    service::NotificationValidationError,
};

pub(super) const MODULE: &str = "llm";
pub(super) const PROVIDERS: &str = "providers";
const PROVIDER_NOTIFICATION: &str = "/llm/config/providers";

#[loki_config]
fn metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: MODULE.into(),
        name: "AI 统一代理".into(),
        description: "配置支持 Codex 或 Claude 协议的上游供应商。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![ConfigItemMetadata {
            key: PROVIDERS.into(),
            name: "供应商".into(),
            description: "为每个协议填写完整的版本化基础 URI 和可选凭据。".into(),
            data_type: ConfigDataType::List,
            remote_access: Default::default(),
            required: false,
            default: Some(json!([])),
            dictionary: None,
            list: Some(ListMetadata {
                item_type: ConfigDataType::Structure,
                min_items: None,
                max_items: None,
            }),
            checker: None,
            file_path: None,
            executable: None,
        }],
    }
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static PROVIDER_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: PROVIDERS,
    options: || ConfigItemOptions {
        notification_endpoint: Some(PROVIDER_NOTIFICATION),
        structure: Some(StructureMetadata {
            id_field: Some("id".into()),
            label_field: Some("id".into()),
            add_label: Some("新增供应商".into()),
            legacy_dictionary: None,
            type_name: "LlmProvider".into(),
            show_all_fields: true,
            fields: vec![
                StructureField {
                    required: true,
                    checker: Some(CheckerMetadata {
                        pattern: "^[A-Za-z0-9_-]{1,64}$".into(),
                        description: "请输入 1 至 64 个字母、数字、下划线或连字符。".into(),
                        emacs_pattern: None,
                    }),
                    ..field("id", "供应商 ID", ConfigDataType::Key)
                },
                field("codex_uri", "Codex 基础 URI", ConfigDataType::String),
                field("claude_uri", "Claude 基础 URI", ConfigDataType::String),
                field(
                    "codex_api_key",
                    "Codex API Key",
                    ConfigDataType::Password {
                        path: "loki-assistant/llm/{key}/codex.gpg".into(),
                        key: None,
                    },
                ),
                field(
                    "claude_api_key",
                    "Claude API Key",
                    ConfigDataType::Password {
                        path: "loki-assistant/llm/{key}/claude.gpg".into(),
                        key: None,
                    },
                ),
            ],
        }),
        ..ConfigItemOptions::default()
    },
};

fn field(key: &str, label: &str, data_type: ConfigDataType) -> StructureField {
    StructureField {
        key: key.into(),
        label: label.into(),
        data_type,
        required: false,
        default_template: None,
        checker: None,
        read_only: false,
        options: None,
    }
}

pub(super) fn provider_uri(record: &Value, field: &str) -> Result<Option<Url>, String> {
    let Some(raw) = record[field]
        .as_str()
        .map(str::trim)
        .filter(|uri| !uri.is_empty())
    else {
        return Ok(None);
    };
    let url = Url::parse(raw)
        .ok()
        .filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
        .ok_or_else(|| format!("{field} 必须是无凭据、查询参数或片段的 HTTP(S) 基础 URI。"))?;
    Ok(Some(url))
}

pub(crate) fn notification_router() -> Router {
    Router::new().route(
        &format!("{PROVIDER_NOTIFICATION}/{{id}}"),
        post(validate_provider).delete(|| async { StatusCode::NO_CONTENT }),
    )
}

async fn validate_provider(Json(record): Json<Value>) -> Response {
    for field in ["codex_uri", "claude_uri"] {
        if let Err(message) = provider_uri(&record, field) {
            let mut response = StatusCode::UNPROCESSABLE_ENTITY.into_response();
            response
                .extensions_mut()
                .insert(NotificationValidationError {
                    code: "llm-provider-uri".into(),
                    message,
                    clear_password_fields: Vec::new(),
                });
            return response;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}
