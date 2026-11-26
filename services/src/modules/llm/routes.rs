use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json,
    extract::{
        Path, Query, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::{
    config::{MonitoringFilters, MonitoringPatch},
    history::{Dimension, Error as HistoryError, OptionsQuery, RequestsQuery, SummaryQuery},
    monitor::{SessionQuery, UsageReport},
    protocol::{Binding, Client, Protocol, SessionUsageRow, UsageRow, error},
    service::LlmService,
};
use crate::http::error::API_VERSION;

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "codex-forward",
    method = "ANY",
    path = "/llm/codex/v1/{*path}",
    summary = "转发 Codex 上游 HTTP 请求",
    description = "按客户端令牌选择供应商并替换鉴权，原样转发请求正文和响应。"
)]
async fn codex(
    State(service): State<Arc<LlmService>>,
    request: Request,
) -> Result<Response, Response> {
    service.forward(Protocol::Codex, request).await
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "claude-forward",
    method = "ANY",
    path = "/llm/claude/v1/{*path}",
    summary = "转发 Claude 上游 HTTP 请求",
    description = "按客户端令牌选择供应商并替换鉴权，原样转发请求正文和响应。"
)]
async fn claude(
    State(service): State<Arc<LlmService>>,
    request: Request,
) -> Result<Response, Response> {
    service.forward(Protocol::Claude, request).await
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "clients",
    method = "GET",
    path = "/llm/clients",
    summary = "查询客户端绑定",
    description = "列出客户端 ID 和供应商。"
)]
async fn clients(
    State(service): State<Arc<LlmService>>,
) -> Result<Json<ClientsResponse>, Response> {
    Ok(Json(ClientsResponse {
        api_version: API_VERSION,
        clients: service.clients().await,
    }))
}

#[derive(serde::Serialize)]
struct ClientsResponse {
    api_version: &'static str,
    clients: Vec<Client>,
}

#[derive(serde::Deserialize)]
struct ClientInput {
    name: Option<String>,
    #[serde(flatten)]
    binding: Binding,
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "provider-ids",
    method = "GET",
    path = "/llm/providers",
    summary = "列出上游供应商",
    description = "返回可用于客户端绑定的供应商 ID。"
)]
async fn provider_ids(State(service): State<Arc<LlmService>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "api_version": API_VERSION, "ids": service.provider_ids().await }))
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "issue-client",
    method = "POST",
    path = "/llm/clients",
    summary = "签发客户端令牌",
    description = "按请求提交的供应商创建绑定，并在响应中一次性交付明文令牌。"
)]
async fn issue_client(
    State(service): State<Arc<LlmService>>,
    body: Result<Json<ClientInput>, JsonRejection>,
) -> Result<Response, Response> {
    let Json(input) = body.map_err(|rejection| {
        error(
            rejection.status(),
            "llm-invalid-binding",
            "请提交供应商和可选的客户端名称。",
        )
    })?;
    let client = service.issue(input.name, input.binding).await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "api_version": API_VERSION, "client": client })),
    )
        .into_response())
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "switch-client",
    method = "PUT",
    path = "/llm/clients/{id}",
    summary = "切换客户端绑定",
    description = "替换客户端的供应商，成功后下一次请求使用新绑定。"
)]
async fn switch_client(
    State(service): State<Arc<LlmService>>,
    Path(id): Path<String>,
    body: Result<Json<Binding>, JsonRejection>,
) -> Result<Json<serde_json::Value>, Response> {
    let client = service.switch(client_id(&id)?, binding(body)?).await?;
    Ok(Json(
        serde_json::json!({ "api_version": API_VERSION, "client": client }),
    ))
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "revoke-client",
    method = "DELETE",
    path = "/llm/clients/{id}",
    summary = "撤销客户端令牌",
    description = "注销指定客户端的令牌，保留按客户端 ID 归档的用量。"
)]
async fn revoke_client(
    State(service): State<Arc<LlmService>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Response> {
    service.revoke(client_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "requests",
    method = "GET",
    path = "/llm/requests",
    summary = "查询实时请求",
    description = "返回正在处理的请求和最近结束的推理请求。"
)]
async fn requests(
    State(service): State<Arc<LlmService>>,
) -> Result<Json<NamedUsage<Value>>, Response> {
    let report = service.monitor.requests().await.map_err(usage_error)?;
    Ok(named_usage(&service, service.archive.annotate(report).await).await)
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "request-detail",
    method = "GET",
    path = "/llm/requests/{id}",
    summary = "查询已采集的请求详情",
    description = "根据请求 ID 从缓存文件读取请求元数据、输入、工具定义和响应。"
)]
async fn request_detail(
    State(service): State<Arc<LlmService>>,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let id = Uuid::parse_str(&id).map_err(|_| usage_error(HistoryError::Invalid))?;
        let mut report = service.archive.detail(id).await.map_err(detail_error)?;
        add_client_name(&mut report["item"], &service.client_names().await);
        Ok::<_, Response>(Json(report))
    };
    let response = match result.await {
        Ok(body) => body.into_response(),
        Err(error) => error,
    };
    ([(header::CACHE_CONTROL, "no-store")], response).into_response()
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "request-detail-raw",
    method = "GET",
    path = "/llm/requests/{id}/raw/{part}",
    summary = "读取请求缓存原始正文",
    description = "按请求 ID 读取 request 或 response 的原始字节流。"
)]
async fn request_detail_raw(
    State(service): State<Arc<LlmService>>,
    Path((id, part)): Path<(String, String)>,
) -> Result<Response, Response> {
    let id = Uuid::parse_str(&id).map_err(|_| usage_error(HistoryError::Invalid))?;
    let response = match part.as_str() {
        "request" => false,
        "response" => true,
        _ => return Err(usage_error(HistoryError::Invalid)),
    };
    let body = service
        .archive
        .raw(id, response)
        .await
        .map_err(detail_error)?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/octet-stream"),
        ],
        body,
    )
        .into_response())
}

#[allow(clippy::needless_pass_by_value)]
fn detail_error(err: std::io::Error) -> Response {
    if err.kind() == std::io::ErrorKind::NotFound {
        error(
            StatusCode::NOT_FOUND,
            "llm-request-detail-not-found",
            "未找到该请求的采集详情。",
        )
    } else {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "llm-request-detail-unavailable",
            "无法读取请求缓存。",
        )
    }
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "monitoring",
    method = "GET",
    path = "/llm/monitoring",
    summary = "读取请求监控配置",
    description = "返回当前过滤条件和最近使用的过滤条件。"
)]
async fn get_monitoring(
    State(service): State<Arc<LlmService>>,
) -> Result<Json<MonitoringFilters>, Response> {
    service.monitoring().await.map(Json).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "llm-monitoring-config-unavailable",
            "无法读取请求监控配置。",
        )
    })
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "update-monitoring",
    method = "PATCH",
    path = "/llm/monitoring",
    summary = "修改请求监控配置",
    description = "设置过滤条件，空条件关闭详情采集，非空条件计入最近使用记录。"
)]
async fn update_monitoring(
    State(service): State<Arc<LlmService>>,
    body: Result<Json<MonitoringPatch>, JsonRejection>,
) -> Result<Json<MonitoringFilters>, Response> {
    let Json(patch) = body.map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "llm-monitoring-config-invalid",
            "请求监控配置格式无效。",
        )
    })?;
    service
        .change_monitoring(patch.filter)
        .await
        .map(Json)
        .map_err(|_| {
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "llm-monitoring-config-unavailable",
                "无法保存请求监控配置。",
            )
        })
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "usage",
    method = "GET",
    path = "/llm/usage",
    summary = "查询代理用量",
    description = "按客户端令牌对应的公开 ID 和供应商汇总输入、输出 token 及用量不完整的请求数。"
)]
async fn usage(State(service): State<Arc<LlmService>>) -> Json<NamedUsage<UsageReport<UsageRow>>> {
    named_usage(&service, service.monitor.usage().await).await
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "session-usage",
    method = "GET",
    path = "/llm/usage/sessions",
    summary = "查询会话用量",
    description = "按客户端 ID、会话标识和供应商汇总输入、输出 token 及用量不完整的请求数。"
)]
async fn session_usage(
    State(service): State<Arc<LlmService>>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<NamedUsage<UsageReport<SessionUsageRow>>>, Response> {
    let report = service
        .monitor
        .session_usage(usage_query(query)?)
        .await
        .map_err(usage_error)?;
    Ok(named_usage(&service, report).await)
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "usage-history",
    method = "GET",
    path = "/llm/usage/requests",
    summary = "查询历史推理请求",
    description = "按时间、客户端、供应商、模型、协议和会话过滤已持久化的请求。"
)]
async fn usage_history(
    State(service): State<Arc<LlmService>>,
    query: Result<Query<RequestsQuery>, QueryRejection>,
) -> Result<Json<NamedUsage<Value>>, Response> {
    let mut report = service
        .monitor
        .history(usage_query(query)?)
        .await
        .map_err(usage_error)?;
    let items = report["items"].as_array_mut().expect("历史记录应为数组");
    let client_names = service.client_names().await;
    for item in items {
        add_client_name(item, &client_names);
    }
    Ok(Json(NamedUsage {
        report,
        client_names,
    }))
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "usage-detail",
    method = "GET",
    path = "/llm/usage/requests/{id}",
    summary = "查询历史请求详情",
    description = "返回归一化用量和上游报告的原始用量。"
)]
async fn usage_detail(
    State(service): State<Arc<LlmService>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, Response> {
    let id = Uuid::parse_str(&id).map_err(|_| usage_error(HistoryError::Invalid))?;
    let mut report = service.monitor.detail(id).await.map_err(usage_error)?;
    add_client_name(&mut report["item"], &service.client_names().await);
    Ok(Json(report))
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "usage-summary",
    method = "GET",
    path = "/llm/usage/summary",
    summary = "查询用量报表",
    description = "按报表时区汇总已持久化的日或月用量。"
)]
async fn usage_summary(
    State(service): State<Arc<LlmService>>,
    query: Result<Query<SummaryQuery>, QueryRejection>,
) -> Result<Json<NamedUsage<Value>>, Response> {
    let report = service
        .monitor
        .summary(usage_query(query)?)
        .await
        .map_err(usage_error)?;
    Ok(named_usage(&service, report).await)
}

#[loki_api(
    module = "llm",
    mount = "Arc<LlmService>",
    id = "usage-options",
    method = "GET",
    path = "/llm/usage/options",
    summary = "查询用量筛选候选",
    description = "查询历史请求或会话中的客户端、供应商或模型。"
)]
async fn usage_options(
    State(service): State<Arc<LlmService>>,
    query: Result<Query<OptionsQuery>, QueryRejection>,
) -> Result<Json<serde_json::Value>, Response> {
    let query = usage_query(query)?;
    let clients = matches!(query.dimension, Dimension::ClientId);
    let mut report = service.monitor.options(query).await.map_err(usage_error)?;
    if clients {
        report["client_names"] = serde_json::json!(service.client_names().await);
    }
    Ok(Json(report))
}

#[derive(Serialize)]
struct NamedUsage<Report> {
    #[serde(flatten)]
    report: Report,
    client_names: BTreeMap<String, String>,
}

async fn named_usage<Report>(service: &LlmService, report: Report) -> Json<NamedUsage<Report>> {
    Json(NamedUsage {
        report,
        client_names: service.client_names().await,
    })
}

fn add_client_name(item: &mut Value, names: &BTreeMap<String, String>) {
    let name = item["client_id"].as_str().and_then(|id| names.get(id));
    item["client_name"] = serde_json::json!(name);
}

fn usage_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, Response> {
    query
        .map(|Query(query)| query)
        .map_err(|_| usage_error(HistoryError::Invalid))
}

fn usage_error(error_value: HistoryError) -> Response {
    match error_value {
        HistoryError::Invalid => error(
            StatusCode::BAD_REQUEST,
            "llm-invalid-usage-query",
            "用量查询参数无效。",
        ),
        HistoryError::NotFound => error(
            StatusCode::NOT_FOUND,
            "llm-usage-request-not-found",
            "未找到该请求。",
        ),
        HistoryError::Store => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "llm-usage-unavailable",
            "无法读取用量数据库。",
        ),
    }
}

fn binding(body: Result<Json<Binding>, JsonRejection>) -> Result<Binding, Response> {
    body.map(|Json(binding)| binding).map_err(|rejection| {
        error(
            rejection.status(),
            "llm-invalid-binding",
            "请提交包含 upstream 的 JSON 对象。",
        )
    })
}

fn client_id(id: &str) -> Result<Uuid, Response> {
    Uuid::parse_str(id).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "llm-invalid-client-id",
            "客户端 ID 必须为 UUID。",
        )
    })
}
