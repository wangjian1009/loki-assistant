use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    middleware,
    response::Response,
};
use loki_services_macros::loki_api;

use super::{
    XbloomService,
    protocol::{self, Create, Detail, Failure, List, Recipe, Request, Saved, Update},
    service::Error,
};
use crate::{
    http::error::{API_VERSION, ErrorSource, response},
    modules::emacs::{json_request::RequestError, service::EmacsError},
};

impl XbloomService {
    pub fn router(&self) -> Router {
        crate::http::mount_routes(Router::new(), "xbloom")
            .method_not_allowed_fallback(|| async {
                response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    ErrorSource::Services,
                    "method-not-allowed",
                    "此 xBloom 接口不支持当前 HTTP 方法。",
                )
            })
            .layer(DefaultBodyLimit::max(64 * 1024))
            .layer(middleware::map_response(
                |mut response: Response| async move {
                    response.headers_mut().insert(
                        header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("no-store"),
                    );
                    response
                },
            ))
            .with_state(self.clone())
    }

    async fn reply<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        request: Request,
    ) -> Result<T, Response> {
        self.call(request).await.map_err(error_response)
    }
}

#[loki_api(
    module = "xbloom",
    mount = "XbloomService",
    id = "list-recipes",
    method = "GET",
    path = "/xbloom/recipes",
    access = "local",
    summary = "查询 xBloom 咖啡配方",
    description = "协议版本 1，合并 Org 配方库和日记配方，支持 q、limit 和 offset。"
)]
async fn list(
    State(service): State<XbloomService>,
    query: Result<Query<protocol::Query>, QueryRejection>,
) -> Result<Json<protocol::Response<List>>, Response> {
    let Query(query) = query.map_err(|_| invalid_request())?;
    let limit = query.limit.unwrap_or(50);
    let q = query.q.map(|value| value.trim().to_owned());
    if !(1..=200).contains(&limit)
        || q.as_ref()
            .is_some_and(|value| value.len() > 512 || value.chars().any(char::is_control))
    {
        return Err(invalid_request());
    }
    let result = service
        .reply(Request::List {
            q: q.filter(|value| !value.is_empty()),
            limit,
            offset: query.offset.unwrap_or_default(),
        })
        .await?;
    Ok(reply(result))
}

#[loki_api(
    module = "xbloom",
    mount = "XbloomService",
    id = "get-recipe",
    method = "GET",
    path = "/xbloom/recipes/{id}",
    access = "local",
    summary = "读取 xBloom 咖啡配方",
    description = "协议版本 1，按 Org ID 读取库中或日记中的配方。"
)]
async fn get(
    State(service): State<XbloomService>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<protocol::Response<Detail>>, Response> {
    let id = path_id(id)?;
    let recipe: Recipe = service.reply(Request::Get { id: id.clone() }).await?;
    verify_id(&recipe, &id)?;
    Ok(reply(Detail { recipe }))
}

#[loki_api(
    module = "xbloom",
    mount = "XbloomService",
    id = "create-recipe",
    method = "POST",
    path = "/xbloom/recipes",
    access = "local",
    summary = "在日记中新建 xBloom 配方",
    description = "协议版本 1，把结构化配方保存到当天的 Org-roam 日记。"
)]
async fn create(
    State(service): State<XbloomService>,
    body: Result<Json<Create>, JsonRejection>,
) -> Result<(StatusCode, Json<protocol::Response<Saved>>), Response> {
    let Json(info) = body.map_err(|_| invalid_request())?;
    if info.based_on_id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err(invalid_request());
    }
    let result = service.reply(Request::Create { info }).await?;
    Ok((StatusCode::CREATED, reply(result)))
}

#[loki_api(
    module = "xbloom",
    mount = "XbloomService",
    id = "update-recipe",
    method = "PUT",
    path = "/xbloom/recipes/{id}",
    access = "local",
    summary = "修改日记中的 xBloom 配方",
    description = "协议版本 1，完整替换日记配方字段并沿用 Org ID。"
)]
async fn update(
    State(service): State<XbloomService>,
    id: Result<Path<String>, PathRejection>,
    body: Result<Json<Update>, JsonRejection>,
) -> Result<Json<protocol::Response<Saved>>, Response> {
    let id = path_id(id)?;
    let Json(Update { recipe }) = body.map_err(|_| invalid_request())?;
    let result: Saved = service
        .reply(Request::Update {
            id: id.clone(),
            recipe,
        })
        .await?;
    verify_id(&result.recipe, &id)?;
    Ok(reply(result))
}

fn reply<T>(result: T) -> Json<protocol::Response<T>> {
    Json(protocol::Response {
        api_version: API_VERSION,
        result,
    })
}

fn path_id(path: Result<Path<String>, PathRejection>) -> Result<String, Response> {
    match path {
        Ok(Path(id)) if valid_id(&id) => Ok(id),
        _ => Err(invalid_request()),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn verify_id(recipe: &Recipe, id: &str) -> Result<(), Response> {
    if recipe.id == id {
        return Ok(());
    }
    Err(crate::modules::emacs::error_response(
        EmacsError::InvalidResponse("xBloom 响应的 Org ID 无效".into()),
    ))
}

fn invalid_request() -> Response {
    error_response(Error::Business(Failure::InvalidRequest))
}

fn error_response(error: Error) -> Response {
    use ErrorSource::{Business, Emacs, Services};
    use Failure::*;
    use StatusCode as S;

    let (status, source, code, message) = match error {
        Error::Request(RequestError::Gateway(error)) => {
            return crate::modules::emacs::error_response(error);
        }
        Error::Request(_) => (
            S::INTERNAL_SERVER_ERROR,
            Services,
            "xbloom-transport-failed",
            "无法将配方请求交给 Emacs。",
        ),
        Error::Business(failure) => match failure {
            InvalidRequest => (
                S::BAD_REQUEST,
                Business,
                "invalid-request",
                "请提供有效的 xBloom 配方或查询参数。",
            ),
            RecipeNotFound => (
                S::NOT_FOUND,
                Business,
                "recipe-not-found",
                "未找到该 Org ID 对应的配方。",
            ),
            RecipeConflict => (
                S::CONFLICT,
                Business,
                "recipe-conflict",
                "多个配方使用了相同的 Org ID。",
            ),
            RecipeReadOnly => (
                S::CONFLICT,
                Business,
                "recipe-read-only",
                "可以此配方为来源，在日记中新建配方。",
            ),
            XbloomUnavailable => (
                S::SERVICE_UNAVAILABLE,
                Emacs,
                "xbloom-unavailable",
                "请在目标 Emacs 中加载 Loki Assistant xBloom 模块和 org-roam。",
            ),
            RecipeReadFailed => (
                S::BAD_GATEWAY,
                Emacs,
                "recipe-read-failed",
                "Emacs 无法读取 Org 配方。",
            ),
            DailyTemplateUnavailable => (
                S::SERVICE_UNAVAILABLE,
                Emacs,
                "daily-template-unavailable",
                "请在 Emacs 中配置指向日记目录的 xBloom 捕获模板。",
            ),
            DailyConflict => (
                S::CONFLICT,
                Business,
                "daily-conflict",
                "日记有尚未保存或加载的更改，请在 Emacs 中处理后重试。",
            ),
            RecipeSaveFailed => (
                S::BAD_GATEWAY,
                Emacs,
                "recipe-save-failed",
                "Emacs 无法保存 xBloom 日记配方。",
            ),
        },
    };
    response(status, source, code, message)
}
