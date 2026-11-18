use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

pub const API_VERSION: &str = "1";

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSource {
    Services,
    Emacs,
    Business,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiError {
    pub source: ErrorSource,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiErrorResponse {
    pub api_version: String,
    pub status: u16,
    pub error: ApiError,
}

pub fn response(
    status: StatusCode,
    source: ErrorSource,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(ApiErrorResponse {
            api_version: API_VERSION.to_owned(),
            status: status.as_u16(),
            error: ApiError {
                source,
                code: code.into(),
                message: message.into(),
            },
        }),
    )
        .into_response()
}
