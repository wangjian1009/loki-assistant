use std::time::SystemTime;

use axum::{http::StatusCode, response::Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::http::error::{ErrorSource, response};

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Protocol {
    Codex,
    Claude,
}

impl Protocol {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::Codex => "/llm/codex/v1",
            Self::Claude => "/llm/claude/v1",
        }
    }

    pub(super) fn uri_field(self) -> &'static str {
        match self {
            Self::Codex => "codex_uri",
            Self::Claude => "claude_uri",
        }
    }

    pub(super) fn key_field(self) -> &'static str {
        match self {
            Self::Codex => "codex_api_key",
            Self::Claude => "claude_api_key",
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Binding {
    pub upstream: String,
}

#[derive(Serialize)]
pub(super) struct Client {
    pub id: Uuid,
    #[serde(flatten)]
    pub binding: Binding,
}

#[derive(Serialize)]
pub(super) struct IssuedClient {
    #[serde(flatten)]
    pub client: Client,
    pub token: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Default, Serialize)]
pub(super) struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub complete: bool,
}

pub(super) struct RequestStatus {
    pub id: Uuid,
    pub client_id: Uuid,
    pub session_id: Option<String>,
    pub upstream: String,
    pub protocol: Protocol,
    pub model: Option<String>,
    pub started_at: SystemTime,
    pub phase: Phase,
    pub first_byte_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub upstream_status: Option<u16>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct UsageTotal {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u64,
    pub incomplete_requests: u64,
}

impl UsageTotal {
    pub(super) fn add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.requests = self.requests.saturating_add(other.requests);
        self.incomplete_requests = self
            .incomplete_requests
            .saturating_add(other.incomplete_requests);
    }

    pub(super) fn record(&mut self, usage: Usage) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(usage.input_tokens.unwrap_or(0));
        self.output_tokens = self
            .output_tokens
            .saturating_add(usage.output_tokens.unwrap_or(0));
        self.requests = self.requests.saturating_add(1);
        self.incomplete_requests = self
            .incomplete_requests
            .saturating_add(u64::from(!usage.complete));
    }
}

#[derive(Serialize)]
pub(super) struct UsageRow {
    pub client_id: Uuid,
    pub upstream: String,
    #[serde(flatten)]
    pub total: UsageTotal,
}

#[derive(Serialize)]
pub(super) struct SessionUsageRow {
    pub session_id: String,
    #[serde(flatten)]
    pub usage: UsageRow,
}

pub(super) fn error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> Response {
    response(status, ErrorSource::Business, code, message)
}
