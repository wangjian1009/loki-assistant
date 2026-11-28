use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Serialize;
use tokio::{
    runtime::Handle,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{
    agent_shell::{
        AgentShell, AgentShellBlock, AgentShellConversationEvent, AgentShellConversationUpdate,
        AgentShellSession,
    },
    error::{CancelledReason, InvalidRequestReason, ResponsError},
    network::{RequestHandle, SseUpdate},
    service_proxy::{ServiceProxy, StatusSubscription},
};

#[derive(Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ConversationPhase {
    #[default]
    Loading,
    Ready,
    Reconnecting,
    Closed,
    Error,
}

#[derive(Default)]
struct ConversationState {
    phase: ConversationPhase,
    blocks: Vec<AgentShellBlock>,
    message: Option<String>,
    display_from: usize,
}

#[derive(Serialize)]
pub(crate) struct ConversationView<'a> {
    phase: &'a ConversationPhase,
    blocks: &'a [AgentShellBlock],
    message: Option<&'a str>,
}

impl ConversationState {
    fn view(&self) -> ConversationView<'_> {
        ConversationView {
            phase: &self.phase,
            blocks: &self.blocks[self.display_from..],
            message: self.message.as_deref(),
        }
    }
}

pub(crate) struct ConversationObserver(JoinHandle<()>);

impl ConversationObserver {
    pub(crate) fn cancel(&self) {
        self.0.abort();
    }
}

impl Drop for ConversationObserver {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct ConversationEntry {
    task: JoinHandle<()>,
    view: watch::Receiver<ConversationState>,
}

impl Drop for ConversationEntry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
struct Conversations {
    generation: u64,
    entries: HashMap<String, ConversationEntry>,
    status: Option<StatusSubscription>,
}

impl Conversations {
    fn select_device(&mut self, generation: u64) {
        if self.generation != generation {
            self.entries.clear();
            self.generation = generation;
        }
    }

    fn retain_sessions(&mut self, ids: &[String]) {
        self.entries.retain(|id, _| ids.contains(id));
        self.stop_watching_if_empty();
    }

    fn stop_watching_if_empty(&mut self) {
        if self.entries.is_empty() {
            self.status.take();
        }
    }
}

type ConversationScope = Mutex<Option<Conversations>>;

pub(crate) struct ConversationSubscriptions {
    proxy: Arc<ServiceProxy>,
    executor: Handle,
    conversations: Arc<ConversationScope>,
}

impl ConversationSubscriptions {
    pub(crate) fn new(proxy: Arc<ServiceProxy>, executor: Handle) -> Self {
        Self {
            proxy,
            executor,
            conversations: Arc::new(Mutex::new(Some(Conversations::default()))),
        }
    }

    fn watch_device(&self) -> StatusSubscription {
        let scope = Arc::downgrade(&self.conversations);
        let target = Arc::downgrade(&self.proxy);
        self.proxy.subscribe(
            |snapshot| {
                (
                    snapshot.generation,
                    AgentShellSession::from_snapshot(snapshot).map(|sessions| {
                        sessions
                            .into_iter()
                            .map(|session| session.id)
                            .collect::<Vec<_>>()
                    }),
                )
            },
            move |(generation, ids)| {
                let (Some(scope), Some(target)) = (scope.upgrade(), target.upgrade()) else {
                    return false;
                };
                let mut scope = scope.lock().unwrap();
                let Some(scope) = scope.as_mut() else {
                    return false;
                };
                if target.snapshot().generation == generation {
                    scope.select_device(generation);
                    if let Some(ids) = ids {
                        scope.retain_sessions(&ids);
                    }
                    scope.stop_watching_if_empty();
                }
                true
            },
        )
    }

    pub(crate) fn observe(
        &self,
        session: String,
        mut changed: impl FnMut(ConversationView<'_>) -> bool + Send + 'static,
    ) -> Result<ConversationObserver, ResponsError> {
        if session.is_empty() || session.chars().any(char::is_control) {
            return Err(InvalidRequestReason::InvalidPath.into());
        }
        let mut conversations = self.conversations.lock().unwrap();
        let scope = conversations
            .as_mut()
            .ok_or(CancelledReason::UserRequested)?;
        let generation = self.proxy.snapshot().generation;
        scope.select_device(generation);
        scope.status.get_or_insert_with(|| self.watch_device());
        let entry = scope.entries.entry(session.clone()).or_insert_with(|| {
            let (publish, view) = watch::channel(ConversationState::default());
            let owner = Arc::downgrade(&self.conversations);
            let proxy = self.proxy.clone();
            let session = session.clone();
            let channel = view.clone();
            let task = self.executor.spawn(async move {
                if monitor(proxy, generation, &session, publish).await
                    && let Some(owner) = owner.upgrade()
                    && let Some(scope) = owner.lock().unwrap().as_mut()
                    && scope
                        .entries
                        .get(&session)
                        .is_some_and(|entry| entry.view.same_channel(&channel))
                {
                    scope.entries.remove(&session);
                    scope.stop_watching_if_empty();
                }
            });
            ConversationEntry { task, view }
        });
        let mut view = entry.view.clone();
        Ok(ConversationObserver(self.executor.spawn(async move {
            loop {
                let listening = changed(view.borrow_and_update().view());
                if !listening || view.changed().await.is_err() {
                    break;
                }
            }
        })))
    }

    #[cfg(target_env = "ohos")]
    pub(crate) async fn preview(
        &self,
        session: String,
    ) -> Result<Vec<AgentShellBlock>, ResponsError> {
        let generation = self.proxy.snapshot().generation;
        {
            let scope = self.conversations.lock().unwrap();
            let scope = scope.as_ref().ok_or(CancelledReason::UserRequested)?;
            if scope.generation == generation
                && let Some(entry) = scope.entries.get(&session)
            {
                let state = entry.view.borrow();
                if state.phase == ConversationPhase::Ready || !state.blocks.is_empty() {
                    return Ok(state.view().blocks.to_vec());
                }
            }
        }
        let shell = AgentShell::new(self.proxy.clone());
        let (send, mut events) = mpsc::unbounded_channel();
        let updates = send.clone();
        let _request = StreamRequest(shell.watch_conversation_for(
            generation,
            session,
            false,
            move |event| {
                let _ = updates.send(Ok(event));
            },
            move |result| {
                if let Err(error) = result {
                    let _ = send.send(Err(error));
                }
            },
        )?);
        tokio::time::timeout(Duration::from_secs(4), async {
            let (publish, view) = watch::channel(ConversationState::default());
            let mut history = ConversationHistory::default();
            while let Some(event) = events.recv().await {
                let event = event?;
                if !matches!(
                    event.update,
                    AgentShellConversationUpdate::Snapshot { .. }
                        | AgentShellConversationUpdate::Update { .. }
                ) || !history.receive(event, &publish)
                {
                    return Err(crate::error::NetworkReason::InvalidResponse.into());
                }
                if history.initialized {
                    return Ok(view.borrow().view().blocks.to_vec());
                }
            }
            Err(crate::error::NetworkReason::InvalidResponse.into())
        })
        .await
        .map_err(|_| ResponsError::from(crate::error::NetworkReason::RequestTimeout))?
    }

    pub(crate) fn close(&self) {
        self.conversations.lock().unwrap().take();
    }
}

impl Drop for ConversationSubscriptions {
    fn drop(&mut self) {
        self.close();
    }
}

enum StreamMessage {
    Stream(SseUpdate<AgentShellConversationEvent>),
    End(Result<(), ResponsError>),
}

struct StreamRequest(RequestHandle);

impl Drop for StreamRequest {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Default)]
struct ConversationHistory {
    revision: Option<u64>,
    cached_revision: Option<u64>,
    replay: Option<(u64, Vec<AgentShellBlock>)>,
    indices: HashMap<String, usize>,
    display_start: Option<String>,
    initialized: bool,
}

impl ConversationHistory {
    fn connect(&mut self, resume: bool) -> Option<u64> {
        self.replay = None;
        self.revision = self.cached_revision.filter(|_| resume);
        self.revision
    }

    fn receive(
        &mut self,
        event: AgentShellConversationEvent,
        view: &watch::Sender<ConversationState>,
    ) -> bool {
        let revision = event.revision;
        match event.update {
            AgentShellConversationUpdate::Snapshot { blocks } => {
                let end = event.snapshot_end_revision.unwrap_or(revision);
                self.replay = Some((end, blocks));
                self.revision = Some(revision);
            }
            AgentShellConversationUpdate::Update { blocks } => {
                if self.revision.and_then(|value| value.checked_add(1)) != Some(revision) {
                    return false;
                }
                self.revision = Some(revision);
                if let Some((_, replay)) = &mut self.replay {
                    replay.extend(blocks);
                } else {
                    self.cached_revision = Some(revision);
                    view.send_modify(|state| {
                        for block in blocks {
                            let position = self
                                .indices
                                .entry(block.id.clone())
                                .or_insert(state.blocks.len());
                            if *position == state.blocks.len() {
                                state.blocks.push(block);
                            } else {
                                state.blocks[*position] = block;
                            }
                        }
                    });
                }
            }
            _ => unreachable!("终止事件由监控处理"),
        }
        if self
            .replay
            .as_ref()
            .is_some_and(|(end, _)| revision >= *end)
            && let Some((_, blocks)) = self.replay.take()
        {
            if !self.initialized {
                self.initialized = true;
                self.display_start = blocks
                    .iter()
                    .rev()
                    .find(|block| block.kind == "user")
                    .map(|block| block.id.clone());
            }
            self.indices = blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.id.clone(), index))
                .collect();
            let display_from = self
                .display_start
                .as_ref()
                .and_then(|id| self.indices.get(id))
                .copied()
                .unwrap_or(0);
            self.cached_revision = Some(revision);
            view.send_replace(ConversationState {
                phase: ConversationPhase::Ready,
                blocks,
                message: None,
                display_from,
            });
        }
        true
    }
}

/// 返回 true 时移除已关闭会话的缓存。
async fn monitor(
    proxy: Arc<ServiceProxy>,
    generation: u64,
    session: &str,
    view: watch::Sender<ConversationState>,
) -> bool {
    let shell = AgentShell::new(proxy.clone());
    let mut retry_delay = Duration::from_millis(500);
    let mut history = ConversationHistory::default();
    let mut resume_supported = true;
    let mut fresh_snapshot = false;
    while proxy.snapshot().generation == generation {
        let after = history.connect(resume_supported && !fresh_snapshot);
        fresh_snapshot = false;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = tx.clone();
        let request = shell.watch_conversation_from(
            generation,
            session.to_owned(),
            after,
            move |update| {
                let _ = events.send(StreamMessage::Stream(update));
            },
            move |end| {
                let _ = tx.send(StreamMessage::End(end));
            },
        );
        let request = match request {
            Ok(request) => StreamRequest(request),
            Err(_) => {
                set_phase(&view, ConversationPhase::Error, Some("无法打开会话".into()));
                return false;
            }
        };

        let mut failure = None;
        while let Some(message) = rx.recv().await {
            match message {
                StreamMessage::Stream(SseUpdate::Opened(_)) => {
                    if after.is_some() {
                        set_phase(&view, ConversationPhase::Ready, None);
                    }
                }
                StreamMessage::Stream(SseUpdate::Event(event)) => match &event.update {
                    AgentShellConversationUpdate::Closed => {
                        view.send_replace(ConversationState {
                            phase: ConversationPhase::Closed,
                            ..Default::default()
                        });
                        return true;
                    }
                    AgentShellConversationUpdate::Error { message } => {
                        failure = message.clone();
                        break;
                    }
                    _ => {
                        if !history.receive(event, &view) {
                            // 连续版本无法衔接时，改用完整快照恢复。
                            fresh_snapshot = after.is_some();
                            break;
                        }
                        if history.replay.is_none() {
                            retry_delay = Duration::from_millis(500);
                        }
                    }
                },
                StreamMessage::End(result) => match result {
                    Err(ResponsError::Services(error))
                        if error.status == 404 && error.code == "agent-shell-session-not-found" =>
                    {
                        view.send_replace(ConversationState {
                            phase: ConversationPhase::Closed,
                            ..Default::default()
                        });
                        return true;
                    }
                    Err(ResponsError::Services(error))
                        if after.is_some()
                            && error.status == 400
                            && error.code == "invalid-request" =>
                    {
                        resume_supported = false;
                        break;
                    }
                    Err(ResponsError::Services(error)) if error.status == 403 => {
                        failure = Some("电脑拒绝读取会话".into());
                        break;
                    }
                    _ => break,
                },
            }
        }
        drop(request);
        if failure.is_some() {
            set_phase(&view, ConversationPhase::Error, failure);
        } else {
            set_phase(&view, ConversationPhase::Reconnecting, None);
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(Duration::from_secs(5));
    }
    false
}

fn set_phase(
    view: &watch::Sender<ConversationState>,
    phase: ConversationPhase,
    message: Option<String>,
) {
    view.send_if_modified(|snapshot| {
        if snapshot.phase == phase && snapshot.message == message {
            return false;
        }
        snapshot.phase = phase;
        snapshot.message = message;
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Network;
    use tokio::sync::oneshot;

    fn seed(
        scope: &ConversationSubscriptions,
        id: &str,
    ) -> (watch::Sender<ConversationState>, oneshot::Receiver<()>) {
        let (publish, view) = watch::channel(ConversationState::default());
        let (live, ended) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _live = live;
            std::future::pending::<()>().await;
        });
        scope
            .conversations
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .entries
            .insert(id.to_owned(), ConversationEntry { task, view });
        (publish, ended)
    }

    #[tokio::test]
    async fn detaching_keeps_the_snapshot_and_removed_sessions_cancel_their_monitor() {
        let network = Arc::new(Network::new());
        let executor = Handle::current();
        let scope =
            ConversationSubscriptions::new(ServiceProxy::new(network, executor.clone()), executor);
        let (session_a, cancelled_a) = seed(&scope, "会话 A");
        let (_session_b, cancelled_b) = seed(&scope, "会话 B");
        let observer = scope.observe("会话 A".to_owned(), |_| true).unwrap();
        observer.cancel();
        set_phase(
            &session_a,
            ConversationPhase::Ready,
            Some("后台更新".into()),
        );

        let (observed, received) = oneshot::channel();
        let mut observed = Some(observed);
        let _observer = scope
            .observe("会话 A".to_owned(), move |view| {
                if let Some(observed) = observed.take() {
                    let _ = observed.send(view.message.map(str::to_owned));
                }
                false
            })
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(1), received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.as_deref(), Some("后台更新"));

        scope
            .conversations
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .retain_sessions(&["会话 B".into()]);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cancelled_a)
                .await
                .unwrap()
                .is_err()
        );
        scope.close();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), cancelled_b)
                .await
                .unwrap()
                .is_err()
        );
        assert!(scope.observe("会话 A".to_owned(), |_| true).is_err());
    }

    fn block(id: &str, kind: &str, text: &str) -> AgentShellBlock {
        AgentShellBlock {
            id: id.into(),
            kind: kind.into(),
            title: None,
            text: text.into(),
            parent_id: None,
            collapsed: None,
        }
    }

    fn event(
        revision: u64,
        end: Option<u64>,
        update: AgentShellConversationUpdate,
    ) -> AgentShellConversationEvent {
        AgentShellConversationEvent {
            session_id: "会话".into(),
            revision,
            snapshot_end_revision: end,
            update,
        }
    }

    #[test]
    fn replay_chooses_the_latest_question_once_and_updates_accumulate_after_it() {
        use AgentShellConversationUpdate::{Snapshot, Update};
        let (publish, view) = watch::channel(ConversationState::default());
        let mut history = ConversationHistory::default();
        assert!(history.receive(
            event(
                5,
                Some(6),
                Snapshot {
                    blocks: vec![block("old", "user", "旧问题")]
                }
            ),
            &publish
        ));
        assert!(!view.has_changed().unwrap());
        assert!(history.receive(
            event(
                6,
                None,
                Update {
                    blocks: vec![
                        block("question", "user", "最近问题"),
                        block("answer", "assistant", "回")
                    ]
                }
            ),
            &publish
        ));
        assert_eq!(view.borrow().view().blocks[0].id, "question");
        assert!(history.receive(
            event(
                7,
                None,
                Update {
                    blocks: vec![
                        block("answer", "assistant", "回答"),
                        block("followup", "user", "追问")
                    ]
                }
            ),
            &publish
        ));
        let stored = view.borrow();
        assert_eq!(stored.blocks.len(), 4);
        assert_eq!(
            stored
                .view()
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["最近问题", "回答", "追问"]
        );
        drop(stored);
        assert_eq!(history.connect(true), Some(7));
        assert!(history.receive(
            event(
                8,
                None,
                Update {
                    blocks: vec![block("answer", "assistant", "补齐的回答")]
                }
            ),
            &publish
        ));
        assert_eq!(view.borrow().view().blocks[1].text, "补齐的回答");
        assert_eq!(view.borrow().view().blocks.len(), 3);
        assert_eq!(history.connect(true), Some(8));
        assert!(history.receive(
            event(
                10,
                Some(11),
                Snapshot {
                    blocks: vec![block("old", "user", "旧问题")]
                }
            ),
            &publish
        ));
        assert_eq!(view.borrow().view().blocks.len(), 3);
        // 断线时，未加载完的快照批次还没有写进会话缓存。
        assert_eq!(history.connect(true), Some(8));
        assert!(!history.receive(event(11, None, Update { blocks: vec![] }), &publish));
        assert!(history.receive(
            event(
                10,
                Some(11),
                Snapshot {
                    blocks: vec![block("old", "user", "旧问题")]
                }
            ),
            &publish
        ));
        assert!(history.receive(
            event(
                11,
                None,
                Update {
                    blocks: vec![
                        block("question", "user", "最近问题"),
                        block("answer", "assistant", "回答"),
                        block("followup", "user", "追问")
                    ]
                }
            ),
            &publish
        ));
        assert_eq!(view.borrow().view().blocks[0].id, "question");
        assert_eq!(history.connect(true), Some(11));
    }
}
