use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureInfo {
    pub title: String,
    pub url: String,
    pub captured_at: String,
    pub body: String,
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(super) enum Request {
    Get { id: String },
    Capture { target: String, info: CaptureInfo },
}

#[derive(Deserialize, Serialize)]
pub(super) struct DocumentSummary {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub domain: String,
    pub node_type: String,
    pub tags: Vec<String>,
    pub date: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Document {
    #[serde(flatten)]
    pub summary: DocumentSummary,
    pub content: String,
}

#[derive(Serialize)]
pub(super) struct DocumentResponse {
    pub api_version: &'static str,
    pub document: Document,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaptureStatus {
    Saved,
    Existing,
}

#[derive(Deserialize, Serialize)]
pub(super) struct CaptureResult {
    pub status: CaptureStatus,
    pub url: String,
    pub document: DocumentSummary,
}

#[derive(Serialize)]
pub(super) struct CaptureResponse {
    pub api_version: &'static str,
    #[serde(flatten)]
    pub result: CaptureResult,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Reply<T> {
    Success { result: T },
    Failure { error: Failure },
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Failure {
    InvalidRequest,
    DocumentNotFound,
    DocumentLocked,
    DocumentReadFailed,
    DocUnavailable,
    CaptureTargetNotFound,
    CaptureTemplateUnavailable,
    CaptureSourceConflict,
    CaptureSaveFailed,
    CaptureIndexFailed,
}
