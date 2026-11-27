use serde::{Deserialize, Serialize};

use crate::service_proxy::{ServiceProxy, ServiceSnapshot, StatusSubscription};
use std::sync::Arc;

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct SlackPreview {
    pub id: String,
    pub title: String,
    pub team_name: String,
    pub unread_count: u64,
    pub preview: String,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct SlackStatus {
    #[serde(default)]
    pub unread_previews: Vec<SlackPreview>,
}

impl SlackStatus {
    pub fn from_snapshot(snapshot: &ServiceSnapshot) -> Option<Self> {
        snapshot.status("slack")
    }
}

#[derive(Clone, PartialEq, Serialize)]
pub struct SlackStatusChanged {
    pub generation: u64,
    pub status: Option<SlackStatus>,
}

pub struct Slack {
    proxy: Arc<ServiceProxy>,
}

impl Slack {
    pub fn new(proxy: Arc<ServiceProxy>) -> Self {
        Self { proxy }
    }

    pub fn watch_status(
        &self,
        changed: impl FnMut(SlackStatusChanged) -> bool + Send + 'static,
    ) -> StatusSubscription {
        self.proxy.subscribe(
            |snapshot| SlackStatusChanged {
                generation: snapshot.generation,
                status: SlackStatus::from_snapshot(snapshot),
            },
            changed,
        )
    }
}
