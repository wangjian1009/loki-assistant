use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::State,
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
        ConfigModuleMetadata, FieldOptions, ListMetadata, StructureField, StructureMetadata,
    },
    registry::ConfigItemRegistration,
    service::{NotificationConfirmedFields, NotificationRequest, NotificationValidationError},
};

pub(super) const MODULE: &str = "llm";
pub(super) const PROVIDERS: &str = "providers";
pub(super) const CLIENTS: &str = "clients";
pub(super) const PROVIDER_TOKEN: &str = "token";
const CLIENT_NOTIFICATION: &str = "/llm/config/clients";
use super::archive::Settings;
use super::service::LlmService;
pub(super) const MONITORING: &str = "monitoring";
const PROVIDER_NOTIFICATION: &str = "/llm/config/providers";

#[loki_config]
fn metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: MODULE.into(),
        name: "AI 统一代理".into(),
        description: "配置支持 Codex 或 Claude 协议的上游供应商。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            collection(
                PROVIDERS,
                "供应商",
                "分别填写协议的版本化基础 URI，并为供应商设置一个可选 Token。",
            ),
            collection(
                CLIENTS,
                "LLM 客户端",
                "填写名称和供应商，保存时自动生成客户端令牌。",
            ),
            ConfigItemMetadata {
                key: MONITORING.into(),
                name: "请求详情监控".into(),
                description: "通过实时请求的监控设置修改范围、保留条数和最近使用的过滤条件。"
                    .into(),
                data_type: ConfigDataType::Dictionary,
                default: Some(json!(Settings::default())),
                dictionary: None,
                remote_access: Default::default(),
                required: false,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
        ],
    }
}

fn collection(key: &str, name: &str, description: &str) -> ConfigItemMetadata {
    ConfigItemMetadata {
        key: key.into(),
        name: name.into(),
        description: description.into(),
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
    }
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static CLIENT_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: CLIENTS,
    options: || ConfigItemOptions {
        notification_endpoint: Some(CLIENT_NOTIFICATION),
        structure: Some(StructureMetadata {
            id_field: Some("name".into()),
            label_field: Some("name".into()),
            add_label: Some("新增 LLM 客户端".into()),
            type_name: "LLMClient".into(),
            show_all_fields: true,
            fields: vec![
                StructureField {
                    required: true,
                    ..field("name", "客户端名称", ConfigDataType::Key)
                },
                StructureField {
                    required: true,
                    options: Some(FieldOptions {
                        path: "/llm/providers".into(),
                        items_field: "ids".into(),
                    }),
                    ..field("upstream", "供应商", ConfigDataType::String)
                },
                StructureField {
                    read_only: true,
                    ..field("token", "客户端 Token", ConfigDataType::String)
                },
            ],
        }),
        ..ConfigItemOptions::default()
    },
};

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
                    PROVIDER_TOKEN,
                    "供应商 Token",
                    ConfigDataType::Password {
                        path: "loki-assistant/llm/{key}.gpg".into(),
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

pub(crate) fn notification_router(service: Arc<LlmService>) -> Router {
    Router::new()
        .route(
            &format!("{PROVIDER_NOTIFICATION}/{{id}}"),
            post(validate_provider).delete(refresh_after_commit),
        )
        .route(
            &format!("{CLIENT_NOTIFICATION}/{{name}}"),
            post(prepare_client).delete(refresh_after_commit),
        )
        .with_state(service)
}

async fn validate_provider(
    State(service): State<Arc<LlmService>>,
    change: Extension<NotificationRequest>,
    Json(record): Json<Value>,
) -> Response {
    for field in ["codex_uri", "claude_uri"] {
        if let Err(message) = provider_uri(&record, field) {
            return invalid("llm-provider-uri", message);
        }
    }
    refresh_after_commit(State(service), change).await
}

async fn prepare_client(
    State(service): State<Arc<LlmService>>,
    Extension(change): Extension<NotificationRequest>,
    Json(record): Json<Value>,
) -> Response {
    if !record["name"]
        .as_str()
        .is_some_and(|name| !name.trim().is_empty())
    {
        return invalid("llm-client-name", "客户端名称不能为空。".into());
    }
    let Some(upstream) = record["upstream"].as_str() else {
        return invalid("llm-provider-not-found", "请选择供应商。".into());
    };
    let generated = match service
        .prepare_client(upstream.to_owned(), change.created.then_some(change.id))
        .await
    {
        Ok(token) => token,
        Err(message) => return invalid("llm-client-validation", message),
    };
    let mut response = refresh_after_commit(State(service), Extension(change)).await;
    if let Some(token) = generated {
        response
            .extensions_mut()
            .insert(NotificationConfirmedFields(
                [("token".into(), json!(token))].into_iter().collect(),
            ));
    }
    response
}

async fn refresh_after_commit(
    State(service): State<Arc<LlmService>>,
    Extension(change): Extension<NotificationRequest>,
) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    change.settle_with(&mut response, move |confirmed| async move {
        if confirmed && let Err(error) = service.reload_authorization().await {
            log::warn!("刷新 LLM 鉴权配置失败：{error}");
        }
    });
    response
}

fn invalid(code: &str, message: String) -> Response {
    let mut response = StatusCode::UNPROCESSABLE_ENTITY.into_response();
    response
        .extensions_mut()
        .insert(NotificationValidationError {
            code: code.into(),
            message,
            clear_password_fields: Vec::new(),
        });
    response
}
