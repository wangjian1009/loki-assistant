use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Request, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;
use uuid::Uuid;

use super::{
    monitor::{Requests, UsageReport},
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
        clients: service.clients().await?,
    }))
}

#[derive(serde::Serialize)]
struct ClientsResponse {
    api_version: &'static str,
    clients: Vec<Client>,
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
    body: Result<Json<Binding>, JsonRejection>,
) -> Result<Response, Response> {
    let client = service.issue(binding(body)?).await?;
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
    summary = "查询代理请求状态",
    description = "列出进行中的请求和最近结束的请求，以及耗时、HTTP 状态和已知用量。"
)]
async fn requests(State(service): State<Arc<LlmService>>) -> Json<Requests> {
    Json(service.monitor.requests().await)
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
async fn usage(State(service): State<Arc<LlmService>>) -> Json<UsageReport<UsageRow>> {
    Json(service.monitor.usage().await)
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
) -> Json<UsageReport<SessionUsageRow>> {
    Json(service.monitor.session_usage().await)
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
