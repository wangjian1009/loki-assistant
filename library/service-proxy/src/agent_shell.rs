use crate::network::{
    InvalidRequestReason, QueryParameter, RequestHandle, ResponsError, ServiceRequest, SseEvent,
    SseUpdate,
};
use crate::service_proxy::{ServiceProxy, ServiceSnapshot, StatusSubscription};
use http::Method;
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
    pub agent_id: Option<String>,
    pub title: Option<String>,
    pub phase: AgentShellPhase,
    pub stop_reason: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellAgent {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
}

#[derive(Deserialize)]
struct AgentShellAgentsResponse {
    agents: Vec<AgentShellAgent>,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellProject {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
}

#[derive(Deserialize)]
struct AgentShellProjectsResponse {
    projects: Vec<AgentShellProject>,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellHistorySession {
    pub id: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
struct AgentShellHistoryResponse {
    sessions: Vec<AgentShellHistorySession>,
}

#[derive(Deserialize)]
struct AgentShellCreatedSession {
    session_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct AgentShellBlock {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub text: String,
    pub parent_id: Option<String>,
    pub collapsed: Option<bool>,
}

#[derive(Deserialize, Serialize)]
pub struct AgentShellConversationEvent {
    pub session_id: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_end_revision: Option<u64>,
    #[serde(flatten)]
    pub update: AgentShellConversationUpdate,
}

#[derive(Deserialize)]
pub struct AgentShellImage {
    pub mime: String,
    pub data: String,
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

    pub async fn list_agents(&self, endpoint: &str) -> Result<Vec<AgentShellAgent>, ResponsError> {
        let generation = self.proxy.bound_generation(endpoint)?;
        Ok(self
            .proxy
            .request::<AgentShellAgentsResponse>(
                generation,
                ServiceRequest::get("/agent-shell/agents"),
            )
            .await?
            .data
            .agents)
    }

    pub async fn list_projects(
        &self,
        endpoint: &str,
    ) -> Result<Vec<AgentShellProject>, ResponsError> {
        let generation = self.proxy.bound_generation(endpoint)?;
        let response = self
            .proxy
            .request::<AgentShellProjectsResponse>(
                generation,
                ServiceRequest::get("/agent-shell/projects"),
            )
            .await?;
        Ok(response.data.projects)
    }

    pub async fn create_session(
        &self,
        endpoint: &str,
        project_id: &str,
        agent_id: &str,
        resume_session_id: Option<&str>,
    ) -> Result<String, ResponsError> {
        let generation = self.proxy.bound_generation(endpoint)?;
        let mut body = serde_json::json!({ "project_id": project_id, "agent_id": agent_id });
        if let Some(id) = resume_session_id {
            body["resume_session_id"] = id.into();
        }
        let request = ServiceRequest {
            method: Method::POST,
            path: "/agent-shell/sessions".into(),
            query: vec![],
            body: Some(body),
        };
        let response = self
            .proxy
            .request::<AgentShellCreatedSession>(generation, request)
            .await?;
        Ok(response.data.session_id)
    }

    pub async fn list_history(
        &self,
        endpoint: &str,
        project_id: &str,
        agent_id: &str,
    ) -> Result<Vec<AgentShellHistorySession>, ResponsError> {
        let generation = self.proxy.bound_generation(endpoint)?;
        let mut request = ServiceRequest::get("/agent-shell/history");
        request.query = vec![
            QueryParameter {
                name: "project_id".into(),
                value: project_id.into(),
            },
            QueryParameter {
                name: "agent_id".into(),
                value: agent_id.into(),
            },
        ];
        let response = self
            .proxy
            .request::<AgentShellHistoryResponse>(generation, request)
            .await?;
        Ok(response.data.sessions)
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
        full: bool,
        changed: impl FnMut(AgentShellConversationEvent) + Send + 'static,
        ended: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> Result<RequestHandle, ResponsError> {
        let generation = self.proxy.snapshot().generation;
        self.watch_conversation_for(generation, session_id, full, changed, ended)
    }

    pub fn watch_conversation_for(
        &self,
        generation: u64,
        session_id: String,
        full: bool,
        mut changed: impl FnMut(AgentShellConversationEvent) + Send + 'static,
        ended: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> Result<RequestHandle, ResponsError> {
        let query = (!full).then(|| QueryParameter {
            name: "recent".into(),
            value: "10".into(),
        });
        self.watch_conversation_with(
            generation,
            session_id,
            query,
            move |update| {
                if let SseUpdate::Event(event) = update {
                    changed(event);
                }
            },
            ended,
        )
    }

    pub fn watch_conversation_from(
        &self,
        generation: u64,
        session_id: String,
        after: Option<u64>,
        changed: impl FnMut(SseUpdate<AgentShellConversationEvent>) + Send + 'static,
        ended: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> Result<RequestHandle, ResponsError> {
        let query = after.map(|revision| QueryParameter {
            name: "after".into(),
            value: revision.to_string(),
        });
        self.watch_conversation_with(generation, session_id, query, changed, ended)
    }

    fn watch_conversation_with(
        &self,
        generation: u64,
        session_id: String,
        query: Option<QueryParameter>,
        mut changed: impl FnMut(SseUpdate<AgentShellConversationEvent>) + Send + 'static,
        ended: impl FnOnce(Result<(), ResponsError>) + Send + 'static,
    ) -> Result<RequestHandle, ResponsError> {
        let path = Self::session_path(&session_id)?;
        let mut request = ServiceRequest::get(format!("{path}/events"));
        request.query.extend(query);
        Ok(self
            .proxy
            .request_sse::<SseEvent<AgentShellConversationEvent>>(
                generation,
                request,
                move |update| match update {
                    SseUpdate::Opened(response) => changed(SseUpdate::Opened(response)),
                    SseUpdate::Event(event)
                        if event.event == "agent-shell" && event.data.session_id == session_id =>
                    {
                        changed(SseUpdate::Event(event.data));
                    }
                    _ => {}
                },
                ended,
            ))
    }

    pub async fn send_message(&self, session_id: &str, text: String) -> Result<(), ResponsError> {
        let request = ServiceRequest {
            method: Method::POST,
            path: format!("{}/messages", Self::session_path(session_id)?),
            query: vec![],
            body: Some(serde_json::json!({ "text": text })),
        };
        self.proxy
            .request::<serde_json::Value>(self.proxy.snapshot().generation, request)
            .await
            .map(|_| ())
    }

    pub async fn restart_session(&self, session_id: &str) -> Result<(), ResponsError> {
        self.post_session_action(session_id, "restart").await
    }

    pub async fn clear_buffer(&self, session_id: &str) -> Result<(), ResponsError> {
        self.post_session_action(session_id, "clear-buffer").await
    }

    async fn post_session_action(
        &self,
        session_id: &str,
        action: &str,
    ) -> Result<(), ResponsError> {
        let request = ServiceRequest::<serde_json::Value> {
            method: Method::POST,
            path: format!("{}/{action}", Self::session_path(session_id)?),
            query: vec![],
            body: None,
        };
        self.proxy
            .request::<serde_json::Value>(self.proxy.snapshot().generation, request)
            .await
            .map(|_| ())
    }

    pub async fn read_image(
        &self,
        generation: u64,
        session_id: &str,
        source: String,
    ) -> Result<AgentShellImage, ResponsError> {
        let mut request = ServiceRequest::get(format!("{}/image", Self::session_path(session_id)?));
        request.query.push(QueryParameter {
            name: "source".into(),
            value: source,
        });
        self.proxy
            .request::<AgentShellImage>(generation, request)
            .await
            .map(|response| response.data)
    }

    pub fn image_target(&self) -> (Option<String>, u64) {
        let snapshot = self.proxy.snapshot();
        (snapshot.endpoint_id, snapshot.generation)
    }

    pub fn has_session(&self, generation: u64, session_id: &str) -> bool {
        let snapshot = self.proxy.snapshot();
        snapshot.generation == generation
            && AgentShellSession::from_snapshot(&snapshot)
                .is_some_and(|sessions| sessions.iter().any(|session| session.id == session_id))
    }

    pub fn watch_image_sessions(
        &self,
        changed: impl FnMut(Option<(String, Vec<String>)>) -> bool + Send + 'static,
    ) -> StatusSubscription {
        self.proxy.subscribe(
            |snapshot| {
                let endpoint = snapshot.endpoint_id.clone()?;
                let sessions = AgentShellSession::from_snapshot(snapshot)?;
                Some((
                    endpoint,
                    sessions.into_iter().map(|session| session.id).collect(),
                ))
            },
            changed,
        )
    }

    fn session_path(session_id: &str) -> Result<String, ResponsError> {
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
        Ok(format!("/agent-shell/sessions/{segment}"))
    }
}
