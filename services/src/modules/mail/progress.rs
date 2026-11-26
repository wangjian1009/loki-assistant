use std::{collections::VecDeque, sync::Arc};

use futures_util::FutureExt;
use serde::Serialize;
use tokio::sync::{Notify, oneshot};
use uuid::Uuid;

use crate::process::Process;

use super::sync::IndexResult;

const RECENT_RUNS: usize = 8;
const RECENT_MESSAGES: usize = 64;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Phase {
    Queued,
    Downloading,
    Indexing,
    Refreshing,
    Completed,
    Failed,
}

#[derive(Clone, Serialize)]
pub(super) struct Snapshot {
    id: Uuid,
    phase: Phase,
    messages: VecDeque<String>,
    error: Option<String>,
}

#[derive(Default)]
struct Runs {
    active: Option<Snapshot>,
    recent: VecDeque<Snapshot>,
    index_reply: Option<(Uuid, oneshot::Sender<IndexResult>)>,
}

impl Runs {
    fn active(&mut self) -> &mut Snapshot {
        self.active.get_or_insert_with(|| Snapshot {
            id: Uuid::new_v4(),
            phase: Phase::Queued,
            messages: VecDeque::new(),
            error: None,
        })
    }
}

#[derive(Debug)]
pub(super) struct SyncProgress {
    runs: Process<Runs>,
    pub(super) requested: Arc<Notify>,
}

impl SyncProgress {
    pub(super) fn new() -> Self {
        Self {
            runs: Process::start(Runs::default()),
            requested: Arc::new(Notify::new()),
        }
    }

    pub(super) async fn request(&self) -> Uuid {
        let requested = Arc::clone(&self.requested);
        self.runs
            .call(move |runs| {
                if runs.active.is_none() {
                    requested.notify_one();
                }
                runs.active().id
            })
            .await
    }

    pub(super) async fn begin(&self) {
        let requested = Arc::clone(&self.requested);
        self.runs
            .call(move |runs| {
                // 定时任务可能已接手同时到达的手动请求。
                let _ = requested.notified().now_or_never();
                runs.active().phase = Phase::Downloading;
            })
            .await;
    }

    pub(super) fn phase(&self, phase: Phase) {
        self.runs.post(move |runs| runs.active().phase = phase);
    }

    pub(super) fn message(&self, message: String) {
        self.runs.post(move |runs| {
            let messages = &mut runs.active().messages;
            if messages.back() == Some(&message) {
                return;
            }
            if messages.len() == RECENT_MESSAGES {
                messages.pop_front();
            }
            messages.push_back(message);
        });
    }

    pub(super) async fn finish(&self, error: Option<String>) {
        self.runs
            .call(move |runs| {
                runs.index_reply = None;
                let mut run = runs.active.take().expect("同步任务已开始");
                run.phase = if error.is_some() {
                    Phase::Failed
                } else {
                    Phase::Completed
                };
                run.error = error;
                if runs.recent.len() == RECENT_RUNS {
                    runs.recent.pop_back();
                }
                runs.recent.push_front(run);
            })
            .await;
    }

    pub(super) async fn expect_index_result(&self, id: Uuid) -> oneshot::Receiver<IndexResult> {
        let (sender, receiver) = oneshot::channel();
        self.runs
            .call(move |runs| runs.index_reply = Some((id, sender)))
            .await;
        receiver
    }

    pub(super) async fn receive_index_result(&self, id: Uuid, result: IndexResult) -> bool {
        self.runs
            .call(move |runs| {
                if runs
                    .index_reply
                    .as_ref()
                    .is_some_and(|(expected, _)| *expected == id)
                {
                    return runs
                        .index_reply
                        .take()
                        .is_some_and(|(_, sender)| sender.send(result).is_ok());
                }
                false
            })
            .await
    }

    pub(super) async fn snapshot(&self, id: Uuid) -> Option<Snapshot> {
        self.runs
            .call(move |runs| {
                runs.active
                    .iter()
                    .chain(&runs.recent)
                    .find(|run| run.id == id)
                    .cloned()
            })
            .await
    }
}
