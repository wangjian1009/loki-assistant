use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use loki_services_macros::loki_config;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use url::Url;
use uuid::Uuid;

use crate::modules::config::{
    model::{
        CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigItemOptions,
        ConfigModuleMetadata, FieldOptions, ListMetadata, StructureField, StructureMetadata,
    },
    registry::ConfigItemRegistration,
    service::{
        ConfigService, ConfigServiceError, NotificationConfirmedFields, NotificationRequest,
        NotificationValidationError,
    },
};

pub(super) const MODULE: &str = "llm";
pub(super) const PROVIDERS: &str = "providers";
pub(super) const CLIENTS: &str = "clients";
pub(super) const PROVIDER_TOKEN: &str = "token";
const CLIENT_NOTIFICATION: &str = "/llm/config/clients";
use super::protocol::Protocol;
use super::service::LlmService;
pub(super) const MONITORING_FILTER: &str = "monitoring_filter";
const MONITORING_HISTORY: &str = "monitoring_history";
pub(super) const MONITORING_RETENTION: &str = "monitoring_retention";
const DEFAULT_RETENTION: usize = 500;
const HISTORY_LIMIT: usize = 10;
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
            setting(
                MONITORING_FILTER,
                "请求详情监控条件",
                "填写 protocols、clients 或 upstreams 数组，条件为空时关闭详情采集。",
                ConfigDataType::Dictionary,
                json!({}),
            ),
            ConfigItemMetadata {
                list: Some(ListMetadata {
                    item_type: ConfigDataType::Dictionary,
                    min_items: None,
                    max_items: Some(HISTORY_LIMIT),
                }),
                ..setting(
                    MONITORING_HISTORY,
                    "请求监控过滤历史",
                    "保存最近使用的过滤条件，可从实时请求列表恢复。",
                    ConfigDataType::List,
                    json!([]),
                )
            },
            setting(
                MONITORING_RETENTION,
                "请求详情保留条数",
                "保留最近的请求详情缓存条数。设为 0 时清理已完成的缓存。",
                ConfigDataType::Integer,
                json!(DEFAULT_RETENTION),
            ),
        ],
    }
}

fn setting(
    key: &str,
    name: &str,
    description: &str,
    data_type: ConfigDataType,
    default: Value,
) -> ConfigItemMetadata {
    ConfigItemMetadata {
        key: key.into(),
        name: name.into(),
        description: description.into(),
        data_type,
        remote_access: Default::default(),
        required: false,
        default: Some(default),
        dictionary: None,
        list: None,
        checker: None,
        file_path: None,
        executable: None,
    }
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static FILTER_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: MONITORING_FILTER,
    options: || ConfigItemOptions {
        validate_value: Some(|value| {
            parse_filter(value.clone())
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
        ..Default::default()
    },
};

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static HISTORY_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: MONITORING_HISTORY,
    options: || ConfigItemOptions {
        validate_value: Some(|value| {
            parse_history(value.clone())
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
        ..Default::default()
    },
};

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static RETENTION_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: MODULE,
    key: MONITORING_RETENTION,
    options: || ConfigItemOptions {
        validate_value: Some(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .map(|_| ())
                .ok_or_else(|| "请求详情保留条数须为可表示的非负整数。".into())
        }),
        ..Default::default()
    },
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Filter {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    protocols: Vec<Protocol>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    clients: Vec<Uuid>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    upstreams: Vec<String>,
}

impl Filter {
    fn normalize(&mut self) {
        self.protocols.sort_unstable();
        self.protocols.dedup();
        self.clients.sort_unstable();
        self.clients.dedup();
        for upstream in &mut self.upstreams {
            *upstream = upstream.trim().to_owned();
        }
        self.upstreams.retain(|upstream| !upstream.is_empty());
        self.upstreams.sort_unstable();
        self.upstreams.dedup();
    }

    fn is_empty(&self) -> bool {
        self.protocols.is_empty() && self.clients.is_empty() && self.upstreams.is_empty()
    }

    pub(super) fn matches(&self, request: &super::protocol::RequestStatus) -> bool {
        !self.is_empty()
            && (self.protocols.is_empty() || self.protocols.contains(&request.protocol))
            && (self.clients.is_empty() || self.clients.contains(&request.client_id))
            && (self.upstreams.is_empty() || self.upstreams.contains(&request.upstream))
    }
}

#[derive(Serialize)]
pub(super) struct MonitoringFilters {
    filter: Filter,
    history: Vec<Filter>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MonitoringPatch {
    pub filter: Filter,
}

fn parse_filter(value: Value) -> Result<Filter, ConfigServiceError> {
    let mut filter: Filter = serde_json::from_value(value)
        .map_err(|_| ConfigServiceError::InvalidValue("请求监控条件格式无效。".into()))?;
    filter.normalize();
    Ok(filter)
}

fn parse_history(value: Value) -> Result<Vec<Filter>, ConfigServiceError> {
    let filters: Vec<Filter> = serde_json::from_value(value)
        .map_err(|_| ConfigServiceError::InvalidValue("请求监控过滤历史格式无效。".into()))?;
    Ok(normalize_history(filters))
}

fn normalize_history(filters: impl IntoIterator<Item = Filter>) -> Vec<Filter> {
    let mut history = Vec::new();
    for mut filter in filters {
        filter.normalize();
        if !filter.is_empty() && !history.contains(&filter) {
            history.push(filter);
            if history.len() == HISTORY_LIMIT {
                break;
            }
        }
    }
    history
}

pub(super) async fn monitoring_filter(
    config: &ConfigService,
) -> Result<Filter, ConfigServiceError> {
    parse_filter(config.value(MODULE, MONITORING_FILTER).await?.value)
}

pub(super) async fn monitoring(
    config: &ConfigService,
) -> Result<MonitoringFilters, ConfigServiceError> {
    let mut values = config.module_values(MODULE).await?;
    Ok(MonitoringFilters {
        filter: parse_filter(
            values
                .remove(MONITORING_FILTER)
                .expect("监控条件有默认值")
                .value,
        )?,
        history: parse_history(
            values
                .remove(MONITORING_HISTORY)
                .expect("过滤历史有默认值")
                .value,
        )?,
    })
}

pub(super) async fn set_monitoring_filter(
    config: &ConfigService,
    mut filter: Filter,
) -> Result<MonitoringFilters, ConfigServiceError> {
    filter.normalize();
    config
        .update_items(MODULE, move |items| {
            let mut history = stored_history(items)?;
            let mut updates = Map::new();
            updates.insert(MONITORING_FILTER.into(), json!(filter));
            if !filter.is_empty() {
                history.retain(|item| item != &filter);
                history.insert(0, filter.clone());
                history.truncate(HISTORY_LIMIT);
                updates.insert(MONITORING_HISTORY.into(), json!(history));
            }
            Ok((updates, MonitoringFilters { filter, history }))
        })
        .await
}

fn stored_history(items: &Map<String, Value>) -> Result<Vec<Filter>, ConfigServiceError> {
    items
        .get(MONITORING_HISTORY)
        .cloned()
        .map(parse_history)
        .transpose()
        .map(Option::unwrap_or_default)
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
        monitor: Some(crate::modules::config::model::ConfigMonitor {
            id: "client",
            endpoint: CLIENT_NOTIFICATION,
        }),
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
                    monitor: Some("client".into()),
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
        monitor: Some(crate::modules::config::model::ConfigMonitor {
            id: "provider",
            endpoint: PROVIDER_NOTIFICATION,
        }),
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
                StructureField {
                    monitor: Some("provider".into()),
                    ..field("codex_uri", "Codex 基础 URI", ConfigDataType::String)
                },
                StructureField {
                    monitor: Some("provider".into()),
                    ..field("claude_uri", "Claude 基础 URI", ConfigDataType::String)
                },
                StructureField {
                    monitor: Some("provider".into()),
                    ..field(
                        PROVIDER_TOKEN,
                        "供应商 Token",
                        ConfigDataType::Password {
                            path: "loki-assistant/llm/{key}.gpg".into(),
                            key: None,
                        },
                    )
                },
            ],
        }),
        ..ConfigItemOptions::default()
    },
};

fn field(key: &str, label: &str, data_type: ConfigDataType) -> StructureField {
    StructureField {
        key: key.into(),
        label: label.into(),
        monitor: None,
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
