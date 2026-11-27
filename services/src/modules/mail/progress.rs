use std::{collections::VecDeque, sync::Arc};

use futures_util::FutureExt;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Notify, oneshot, watch};
use uuid::Uuid;

use crate::{modules::status::StatusService, process::Process};

use super::{
    MODULE_ID,
    status::SYNC_RUNS,
    sync::{IndexUpdate, MuDatabase},
};

const RECENT_RUNS: usize = 8;
const RECENT_MESSAGES: usize = 64;

#[derive(Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Queued,
    Downloading,
    Indexing,
    Refreshing,
    Completed,
    Failed,
}

#[derive(Clone, Serialize, JsonSchema)]
pub(super) struct Snapshot {
    pub(super) id: Uuid,
    phase: Phase,
    messages: VecDeque<String>,
    error: Option<String>,
}

impl Snapshot {
    fn summary(&self) -> serde_json::Value {
        json!({"id": self.id, "phase": self.phase})
    }
}

struct Runs {
    status: Arc<StatusService>,
    active: Option<watch::Sender<Snapshot>>,
    recent: VecDeque<Snapshot>,
    index_reply: Option<(Uuid, oneshot::Sender<Result<MuDatabase, String>>)>,
}

impl Runs {
    fn active(&mut self) -> &watch::Sender<Snapshot> {
        self.active.get_or_insert_with(|| {
            watch::Sender::new(Snapshot {
                id: Uuid::new_v4(),
                phase: Phase::Queued,
                messages: VecDeque::new(),
                error: None,
            })
        })
    }

    fn publish(&self) {
        let summaries: Vec<_> = self
            .active
            .iter()
            .map(|run| run.borrow().summary())
            .chain(self.recent.iter().map(Snapshot::summary))
            .collect();
        self.status.post(MODULE_ID, SYNC_RUNS, json!(summaries));
    }

    fn message(&mut self, message: String) {
        self.active().send_if_modified(|run| {
            let messages = &mut run.messages;
            if messages.back() == Some(&message) {
                return false;
            }
            if messages.len() == RECENT_MESSAGES {
                messages.pop_front();
            }
            messages.push_back(message);
            true
        });
    }
}

#[derive(Debug)]
pub(super) struct SyncProgress {
    runs: Process<Runs>,
    pub(super) requested: Arc<Notify>,
}

impl SyncProgress {
    pub(super) fn new(status: Arc<StatusService>) -> Self {
        status.post(MODULE_ID, SYNC_RUNS, json!([]));
        Self {
            runs: Process::start(Runs {
                status,
                active: None,
                recent: VecDeque::new(),
                index_reply: None,
            }),
            requested: Arc::new(Notify::new()),
        }
    }

    pub(super) async fn request(&self) -> (Snapshot, watch::Receiver<Snapshot>) {
        let requested = Arc::clone(&self.requested);
        self.runs
            .call(move |runs| {
                if runs.active.is_none() {
                    runs.active();
                    runs.publish();
                    requested.notify_one();
                }
                let run = runs.active();
                (run.borrow().clone(), run.subscribe())
            })
            .await
    }

    pub(super) async fn begin(&self) {
        let requested = Arc::clone(&self.requested);
        self.runs
            .call(move |runs| {
                // 定时任务可能已接手同时到达的手动请求。
                let _ = requested.notified().now_or_never();
                runs.active()
                    .send_modify(|run| run.phase = Phase::Downloading);
                runs.publish();
            })
            .await;
    }

    pub(super) fn phase(&self, phase: Phase) {
        self.runs.post(move |runs| {
            runs.active().send_modify(|run| run.phase = phase);
            runs.publish();
        });
    }

    pub(super) fn message(&self, message: String) {
        self.runs.post(move |runs| runs.message(message));
    }

    pub(super) async fn finish(&self, error: Option<String>) {
        self.runs
            .call(move |runs| {
                runs.index_reply = None;
                let run = runs.active.take().expect("同步任务已开始");
                run.send_modify(|run| {
                    run.phase = if error.is_some() {
                        Phase::Failed
                    } else {
                        Phase::Completed
                    };
                    run.error = error;
                });
                if runs.recent.len() == RECENT_RUNS {
                    runs.recent.pop_back();
                }
                runs.recent.push_front(run.borrow().clone());
                runs.publish();
            })
            .await;
    }

    pub(super) async fn expect_index_result(
        &self,
        id: Uuid,
    ) -> oneshot::Receiver<Result<MuDatabase, String>> {
        let (sender, receiver) = oneshot::channel();
        self.runs
            .call(move |runs| runs.index_reply = Some((id, sender)))
            .await;
        receiver
    }

    pub(super) async fn receive_index_update(&self, id: Uuid, update: IndexUpdate) -> bool {
        self.runs
            .call(move |runs| {
                if !runs
                    .index_reply
                    .as_ref()
                    .is_some_and(|(expected, sender)| *expected == id && !sender.is_closed())
                {
                    return false;
                }
                let result = match update {
                    IndexUpdate::Running { index } => {
                        runs.message(index.message());
                        return true;
                    }
                    IndexUpdate::Completed { database } => Ok(database),
                    IndexUpdate::Failed { error } => Err(error),
                };
                runs.index_reply
                    .take()
                    .is_some_and(|(_, sender)| sender.send(result).is_ok())
            })
            .await
    }

    pub(super) async fn snapshot(&self, id: Uuid) -> Option<Snapshot> {
        self.runs
            .call(move |runs| {
                if let Some(run) = &runs.active {
                    let run = run.borrow();
                    if run.id == id {
                        return Some(run.clone());
                    }
                }
                runs.recent.iter().find(|run| run.id == id).cloned()
            })
            .await
    }
}
