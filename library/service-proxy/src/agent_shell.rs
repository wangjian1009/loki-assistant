use crate::service_proxy::{ServiceProxy, StatusSubscription};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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

#[derive(Deserialize)]
struct EmacsStatus {
    agent_shell: Option<Vec<AgentShellSession>>,
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
        self.proxy.subscribe(
            |snapshot| {
                snapshot
                    .status::<EmacsStatus>("emacs")
                    .and_then(|status| status.agent_shell)
            },
            changed,
        )
    }
}
