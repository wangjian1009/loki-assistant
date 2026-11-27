use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    network::{
        ApiVersion, CancelledReason, NetworkReason, Outcome, ResponsError, ServiceRequest,
        SseEventType, SseUpdate,
    },
    service_proxy::{ServiceProxy, ServiceSnapshot, StatusSubscription},
};

const MODULE: &str = "mail";
const SYNC_PATH: &str = "/mail/sync";
const ACTIVITY: &str = "sync_activity";

pub struct MailSyncRequest;

impl MailSyncRequest {
    fn request() -> ServiceRequest<()> {
        ServiceRequest {
            method: http::Method::POST,
            path: SYNC_PATH.into(),
            query: Vec::new(),
            body: None,
        }
    }
}

#[derive(Deserialize)]
pub struct MailSyncAccepted {
    #[serde(rename = "api_version")]
    _api_version: ApiVersion,
    pub id: Uuid,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailSyncPhase {
    Queued,
    Downloading,
    Indexing,
    Refreshing,
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

impl MailSyncPhase {
    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct MailSyncProgress {
    pub id: Uuid,
    pub phase: MailSyncPhase,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct MailSyncSnapshot {
    #[serde(flatten)]
    pub progress: MailSyncProgress,
    #[serde(default)]
    pub messages: Vec<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct MailSyncResponse {
    #[serde(rename = "api_version")]
    _api_version: ApiVersion,
    pub sync: MailSyncSnapshot,
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MailSyncEvent {
    Sync {
        data: MailSyncResponse,
    },
    #[serde(other)]
    Unsupported,
}

impl SseEventType for MailSyncEvent {
    fn supported(&self) -> bool {
        matches!(self, Self::Sync { data } if data.sync.progress.phase != MailSyncPhase::Unknown)
    }
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct MailStatus {
    pub unread_count: Option<u64>,
    #[serde(default)]
    pub unread_subjects: Vec<String>,
    #[serde(default)]
    pub sync_runs: Vec<MailSyncProgress>,
}

impl MailStatus {
    pub fn from_snapshot(snapshot: &ServiceSnapshot) -> Option<Self> {
        snapshot.status(MODULE)
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct MailActivity {
    pub sync: Option<MailSyncSnapshot>,
    pub receiving: bool,
    pub error: Option<ResponsError>,
}

impl MailActivity {
    pub fn from_snapshot(snapshot: &ServiceSnapshot) -> Option<Self> {
        let mut activity: Self = snapshot.local(MODULE, ACTIVITY)?;
        if !activity.receiving
            && let Some(sync) = &mut activity.sync
            && !sync.progress.phase.terminal()
            && snapshot.error.is_none()
            && let Some(status) = MailStatus::from_snapshot(snapshot)
            && let Some(progress) = status.sync_runs.into_iter().find(|run| {
                run.id == sync.progress.id
                    && run.phase != MailSyncPhase::Unknown
                    && run.phase >= sync.progress.phase
            })
        {
            sync.progress = progress;
        }
        Some(activity)
    }
}

#[derive(Clone, PartialEq, Serialize)]
pub struct MailStatusChanged {
    pub endpoint_id: Option<String>,
    pub generation: u64,
    pub status: Option<MailStatus>,
    pub activity: Option<MailActivity>,
}

#[derive(Clone)]
pub struct Mail {
    proxy: Arc<ServiceProxy>,
    active_generation: Arc<Mutex<Option<u64>>>,
}

impl Mail {
    pub fn new(proxy: Arc<ServiceProxy>) -> Self {
        Self {
            proxy,
            active_generation: Default::default(),
        }
    }

    pub fn watch_status(
        &self,
        changed: impl FnMut(MailStatusChanged) -> bool + Send + 'static,
    ) -> StatusSubscription {
        self.proxy.subscribe(
            |snapshot| MailStatusChanged {
                endpoint_id: snapshot.endpoint_id.clone(),
                generation: snapshot.generation,
                status: MailStatus::from_snapshot(snapshot),
                activity: MailActivity::from_snapshot(snapshot),
            },
            changed,
        )
    }

    pub fn start_sync(&self, endpoint: &str) -> Result<(), ResponsError> {
        let mut active = self.active_generation.lock().unwrap();
        let generation = self.proxy.bound_generation(endpoint)?;
        if *active == Some(generation) {
            return Ok(());
        }
        *active = Some(generation);
        self.proxy.sync_target();
        self.publish(generation, None, true, None);
        drop(active);
        let mail = self.clone();
        self.proxy.spawn(async move {
            let result = mail.sync(generation).await;
            let mut active = mail.active_generation.lock().unwrap();
            if *active == Some(generation) {
                *active = None;
                if let Err(error) = result {
                    mail.publish(generation, None, false, Some(error));
                }
            }
        });
        Ok(())
    }

    async fn sync(&self, generation: u64) -> Result<(), ResponsError> {
        let sse = match self.proxy.api_metadata(generation, MODULE).await {
            Ok(meta) => meta.apis.iter().any(|api| {
                api.method == "POST"
                    && api.path == SYNC_PATH
                    && api.supports_sse
                    && api.sse_events.iter().any(|event| event.event == "sync")
            }),
            // 较旧的服务可能尚未提供邮件模块的发现响应。
            Err(ResponsError::Services(error)) if error.status == 404 => false,
            Err(error) => return Err(error),
        };
        if !sse {
            let response = self
                .proxy
                .request::<MailSyncAccepted>(generation, MailSyncRequest::request())
                .await?;
            if response.status != http::StatusCode::ACCEPTED {
                return Err(NetworkReason::InvalidResponse.into());
            }
            let sync = MailSyncSnapshot {
                progress: MailSyncProgress {
                    id: response.data.id,
                    phase: MailSyncPhase::Queued,
                },
                messages: Vec::new(),
                error: None,
            };
            self.publish(generation, Some(sync.clone()), false, None);
            return self.follow_status(generation, sync).await;
        }

        let (done, ended) = oneshot::channel();
        let mail = self.clone();
        self.proxy.request_sse::<MailSyncEvent>(
            generation,
            MailSyncRequest::request(),
            move |update| {
                if let SseUpdate::Event(MailSyncEvent::Sync { data }) = update {
                    mail.publish(generation, Some(data.sync), true, None);
                }
            },
            move |result| {
                let _ = done.send(result);
            },
        );
        let result = ended
            .await
            .map_err(|_| ResponsError::from(NetworkReason::ReceiveFailed))?;
        let snapshot = self.proxy.snapshot();
        if let Some(sync) = (snapshot.generation == generation)
            .then(|| MailActivity::from_snapshot(&snapshot))
            .flatten()
            .and_then(|activity| activity.sync)
        {
            self.publish(generation, Some(sync.clone()), false, None);
            return self.follow_status(generation, sync).await;
        }
        // POST 可能已经受理。先读取状态中仍在运行的同步任务，避免重复提交。
        if result
            .as_ref()
            .err()
            .is_none_or(|error| error.outcome() == Outcome::Unknown)
        {
            let snapshot = self.proxy.refresh_status_for(generation).await?;
            if snapshot.error.is_none()
                && let Some(progress) = MailStatus::from_snapshot(&snapshot).and_then(|status| {
                    status
                        .sync_runs
                        .into_iter()
                        .find(|run| !run.phase.terminal() && run.phase != MailSyncPhase::Unknown)
                })
            {
                let sync = MailSyncSnapshot {
                    progress,
                    messages: Vec::new(),
                    error: None,
                };
                self.publish(generation, Some(sync.clone()), false, None);
                return self.follow_status(generation, sync).await;
            }
        }
        result.and(Err(NetworkReason::InvalidResponse.into()))
    }

    async fn follow_status(
        &self,
        generation: u64,
        mut sync: MailSyncSnapshot,
    ) -> Result<(), ResponsError> {
        let mut snapshots = self.proxy.status_updates();
        let follow = async {
            while !sync.progress.phase.terminal() {
                let snapshot = snapshots.borrow_and_update().clone();
                if snapshot.generation != generation {
                    return Err(CancelledReason::ServiceChanged.into());
                }
                if let Some(error @ ResponsError::NotBound(_)) = snapshot.error {
                    return Err(error);
                }
                if let Some(updated) =
                    MailActivity::from_snapshot(&snapshot).and_then(|activity| activity.sync)
                    && updated != sync
                {
                    sync = updated;
                    self.publish(generation, Some(sync.clone()), false, None);
                    if sync.progress.phase.terminal() {
                        break;
                    }
                }
                snapshots
                    .changed()
                    .await
                    .map_err(|_| CancelledReason::NetworkClosed)?;
            }
            Ok(())
        };
        match tokio::time::timeout(Duration::from_secs(300), follow).await {
            Ok(result) => result,
            Err(_) => {
                self.publish(
                    generation,
                    Some(sync),
                    false,
                    Some(NetworkReason::RequestTimeout.into()),
                );
                Ok(())
            }
        }
    }

    fn publish(
        &self,
        generation: u64,
        sync: Option<MailSyncSnapshot>,
        receiving: bool,
        error: Option<ResponsError>,
    ) {
        self.proxy.publish_local(
            generation,
            MODULE,
            ACTIVITY,
            &MailActivity {
                sync,
                receiving,
                error,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    #[test]
    fn mail_event_ignores_added_fields_and_unknown_event_kinds() {
        let unknown: MailSyncEvent = from_value(json!({
            "event": "future_event", "data": {"future_payload": [1, 2, 3]}
        }))
        .unwrap();
        assert!(!unknown.supported());

        let mut event = json!({
            "event": "sync", "new_header": true, "data": {
                "api_version": "1", "extra": "new server version", "sync": {
                    "id": "550e8400-e29b-41d4-a716-446655440000", "phase": "indexing",
                    "messages": ["正在编制索引"], "error": null, "future_field": 42
                }
            }
        });
        let decoded: MailSyncEvent = from_value(event.clone()).unwrap();
        assert!(decoded.supported());
        let MailSyncEvent::Sync { data } = decoded else {
            panic!("同步事件未恢复为类型");
        };
        assert_eq!(data.sync.messages, ["正在编制索引"]);

        event["data"]["sync"]["phase"] = json!("future_phase");
        let decoded: MailSyncEvent = from_value(event).unwrap();
        assert!(!decoded.supported());
    }
}
