use crate::network::{
    InvalidRequestReason, RequestHandle, ResponsError, ServiceRequest, SseEvent, SseUpdate,
};
use crate::service_proxy::{ServiceProxy, ServiceSnapshot, StatusSubscription};
use serde::{Deserialize, Serialize};
use std::{fmt::Write as _, sync::Arc};

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentShellPhase {
    Idle,
    Starting,
    Running,
    WaitingInput,
    Completed,
    Cancelled,
    Failed,
    Disconnected,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct AgentShellSession {
    pub id: String,
    pub title: Option<String>,
    pub phase: AgentShellPhase,
    pub stop_reason: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellBlock {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub text: String,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellConversationEvent {
    pub session_id: String,
    pub revision: u64,
    #[serde(flatten)]
    pub update: AgentShellConversationUpdate,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentShellConversationUpdate {
    Snapshot { blocks: Vec<AgentShellBlock> },
    Update { blocks: Vec<AgentShellBlock> },
    Closed,
    Error { message: Option<String> },
}

#[derive(Deserialize)]
struct EmacsStatus {
    agent_shell: Option<Vec<AgentShellSession>>,
}

impl AgentShellSession {
    pub fn from_snapshot(snapshot: &ServiceSnapshot) -> Option<Vec<Self>> {
        snapshot
            .status::<EmacsStatus>("emacs")
            .and_then(|status| status.agent_shell)
    }
}

pub struct AgentShell {
    proxy: Arc<ServiceProxy>,
}

impl AgentShell {
    pub fn new(proxy: Arc<ServiceProxy>) -> Self {
        Self { proxy }
    }

    pub fn watch_status(
        &self,
        changed: impl FnMut(Option<Vec<AgentShellSession>>) -> bool + Send + 'static,
    ) -> StatusSubscription {
        self.proxy
            .subscribe(AgentShellSession::from_snapshot, changed)
    }

    pub fn watch_conversation(
        &self,
        session_id: String,
        mut changed: impl FnMut(AgentShellConversationEvent) + Send + 'static,
        ended: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> Result<RequestHandle, ResponsError> {
        if session_id.is_empty() || session_id.chars().any(char::is_control) {
            return Err(InvalidRequestReason::InvalidPath.into());
        }
        let mut segment = String::with_capacity(session_id.len());
        for byte in session_id.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                segment.push(char::from(byte));
            } else {
                let _ = write!(segment, "%{byte:02X}");
            }
        }
        let generation = self.proxy.snapshot().generation;
        let request = ServiceRequest::get(format!("/agent-shell/sessions/{segment}/events"));
        Ok(self
            .proxy
            .request_sse::<SseEvent<AgentShellConversationEvent>>(
                generation,
                request,
                move |update| {
                    if let SseUpdate::Event(event) = update
                        && event.event == "agent-shell"
                        && event.data.session_id == session_id
                    {
                        changed(event.data);
                    }
                },
                ended,
            ))
    }
}
