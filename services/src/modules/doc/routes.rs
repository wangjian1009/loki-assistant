use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
};
use loki_services_macros::loki_api;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use super::{
    DocService,
    protocol::{
        CaptureInfo, CaptureResponse, CaptureResult, CaptureStatus, Document, DocumentResponse,
        Failure, Request,
    },
    service::{DocError, invalid_reply},
};
use crate::http::error::{API_VERSION, ErrorSource, response};

const LOCATION_ID: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-');

impl DocService {
    pub fn router(&self) -> Router {
        crate::http::mount_routes(Router::new(), "doc")
            .method_not_allowed_fallback(method_not_allowed)
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024 + 4096))
            .layer(middleware::map_response(no_store))
            .with_state(self.clone())
    }
}

#[loki_api(
    module = "doc",
    mount = "DocService",
    id = "get-document",
    method = "GET",
    path = "/doc/{id}",
    access = "local",
    summary = "按 ID 读取 Org 文档",
    description = "协议版本 1，按 Org ID 返回已保存的文件或标题子树。"
)]
async fn get_document(
    State(service): State<DocService>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<DocumentResponse>, Response> {
    let id = path_segment(id)?;
    let document: Document = service
        .call(Request::Get { id: id.clone() })
        .await
        .map_err(error_response)?;
    if document.summary.id != id {
        return Err(error_response(invalid_reply()));
    }
    Ok(Json(DocumentResponse {
        api_version: API_VERSION,
        document,
    }))
}

#[loki_api(
    module = "doc",
    mount = "DocService",
    id = "capture-full",
    method = "POST",
    path = "/doc/{id}",
    access = "local",
    summary = "按目标保存网页全文",
    description = "协议版本 1，路径段为 Emacs 提交目标，当前支持 article。"
)]
async fn capture(
    State(service): State<DocService>,
    target: Result<Path<String>, PathRejection>,
    info: Result<Json<CaptureInfo>, JsonRejection>,
) -> Result<Response, Response> {
    let target = path_segment(target)?;
    let Json(info) =
        info.map_err(|_| error_response(DocError::Business(Failure::InvalidRequest)))?;
    let url = info.url.clone();
    let result: CaptureResult = service
        .call(Request::Capture { target, info })
        .await
        .map_err(error_response)?;
    if result.url != url || !valid_segment(&result.document.id) {
        return Err(error_response(invalid_reply()));
    }
    let status = match result.status {
        CaptureStatus::Saved => StatusCode::CREATED,
        CaptureStatus::Existing => StatusCode::OK,
    };
    let location = (status == StatusCode::CREATED).then(|| {
        let id = utf8_percent_encode(&result.document.id, LOCATION_ID);
        HeaderValue::from_str(&format!("/doc/{id}")).expect("编码后的 Org ID 可用作响应头")
    });
    let mut response = (
        status,
        Json(CaptureResponse {
            api_version: API_VERSION,
            result,
        }),
    )
        .into_response();
    if let Some(location) = location {
        response.headers_mut().insert(header::LOCATION, location);
    }
    Ok(response)
}

fn valid_segment(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn path_segment(path: Result<Path<String>, PathRejection>) -> Result<String, Response> {
    match path {
        Ok(Path(value)) if valid_segment(&value) => Ok(value),
        _ => Err(response(
            StatusCode::BAD_REQUEST,
            ErrorSource::Services,
            "invalid-request",
            "请提供有效的文档 ID 或提交目标。",
        )),
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn method_not_allowed() -> Response {
    response(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorSource::Services,
        "method-not-allowed",
        "文档接口支持 GET 查询和 POST 提交。",
    )
}

fn error_response(error: DocError) -> Response {
    use ErrorSource::{Business, Emacs, Services};
    use Failure::*;

    let (status, source, code, message) = match error {
        DocError::Gateway(error) => return crate::modules::emacs::error_response(error),
        DocError::Transport => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Services,
            "doc-transport-failed",
            "无法将文档请求交给 Emacs。",
        ),
        DocError::Business(failure) => match failure {
            InvalidRequest => (
                StatusCode::BAD_REQUEST,
                Business,
                "invalid-request",
                "请提供有效的网页标题、HTTP(S) URL、UTC 抓取时间和非空 Org 正文。",
            ),
            DocumentNotFound => (
                StatusCode::NOT_FOUND,
                Business,
                "document-not-found",
                "未找到该 ID 对应的 Org 文档。",
            ),
            DocumentLocked => (
                StatusCode::FORBIDDEN,
                Business,
                "document-locked",
                "请先在 Emacs 中解锁该文档，再重试。",
            ),
            DocumentReadFailed => (
                StatusCode::BAD_GATEWAY,
                Emacs,
                "document-read-failed",
                "Emacs 无法读取文档，请检查 org-roam 索引和文件。",
            ),
            DocUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Emacs,
                "doc-unavailable",
                "请在目标 Emacs 中加载 Loki Assistant 文档模块和 org-roam。",
            ),
            CaptureTargetNotFound => (
                StatusCode::NOT_FOUND,
                Business,
                "capture-target-not-found",
                "Emacs 不支持此全文捕获目标。",
            ),
            CaptureTemplateUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                Emacs,
                "capture-template-unavailable",
                "请在 Emacs 的文档模块中配置该目标的全文捕获模板。",
            ),
            CaptureSourceConflict => (
                StatusCode::CONFLICT,
                Business,
                "capture-source-conflict",
                "该网页对应多篇文章，请在 Emacs 中确认它们的来源。",
            ),
            CaptureSaveFailed => (
                StatusCode::BAD_GATEWAY,
                Emacs,
                "capture-save-failed",
                "Emacs 无法保存网页全文，请检查捕获模板和目标目录。",
            ),
            CaptureIndexFailed => (
                StatusCode::BAD_GATEWAY,
                Emacs,
                "capture-index-failed",
                "Emacs 无法更新网页全文的索引，请检查 org-roam 后重试。",
            ),
        },
    };
    response(status, source, code, message)
}
