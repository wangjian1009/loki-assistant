use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    sync::{oneshot, watch},
    task::JoinSet,
    time::sleep,
};
use uuid::Uuid;

use crate::modules::emacs::service::EmacsService;

use super::{
    mailbox::next_cursor,
    protocol::{Error, Message, timestamp},
    service::SlackService,
    store::Cache,
};

const JOURNAL_LIMIT: usize = 512;
const PAGE_SIZE: usize = 50;
const IDLE_PROBE: Duration = Duration::from_secs(60);

pub(super) type SubscriptionKey = (String, Uuid);

#[derive(Default)]
pub(super) struct MessageJournal {
    changed: watch::Sender<u64>,
    entries: VecDeque<(u64, String, Option<Message>)>,
}

impl MessageJournal {
    pub fn observe(&self) -> (watch::Receiver<u64>, u64) {
        (self.changed.subscribe(), *self.changed.borrow())
    }

    pub fn changed_since(
        &self,
        (observer, revision): &(watch::Receiver<u64>, u64),
    ) -> Option<BTreeSet<String>> {
        if !observer.same_channel(&self.changed.subscribe()) || self.overflowed(*revision) {
            return None;
        }
        Some(
            self.entries
                .iter()
                .filter(|(index, _, _)| index > revision)
                .map(|(_, ts, _)| ts.clone())
                .collect(),
        )
    }

    fn overflowed(&self, revision: u64) -> bool {
        self.entries
            .front()
            .is_some_and(|(first, _, _)| *first > revision + 1)
    }

    pub fn push(&mut self, ts: String, message: Option<Message>) {
        let revision = *self.changed.borrow() + 1;
        self.entries.push_back((revision, ts, message));
        if self.entries.len() > JOURNAL_LIMIT {
            self.entries.pop_front();
        }
        self.changed.send_replace(revision);
    }
}

pub(super) struct Subscription {
    registration: Uuid,
    pending: Option<Delivery>,
    _cancel: oneshot::Sender<()>,
}

struct Delivery {
    session: Uuid,
    changes: watch::Receiver<u64>,
    cancel: oneshot::Receiver<()>,
}

impl Cache {
    pub fn clear_subscriptions(&mut self) {
        self.subscriptions.clear();
        self.conversations
            .values_mut()
            .for_each(|room| room.journal = None);
    }

    fn unsubscribe(&mut self, key: &SubscriptionKey, registration: Option<Uuid>) {
        if self.subscriptions.get(key).is_none_or(|entry| {
            registration.is_some_and(|registration| entry.registration != registration)
        }) {
            return;
        }
        self.subscriptions.remove(key);
        if !self.subscriptions.keys().any(|(id, _)| id == &key.0)
            && let Some(room) = self.conversations.get_mut(&key.0)
        {
            room.journal = None;
        }
    }
}

impl SlackService {
    pub(super) async fn subscribe_messages(
        &self,
        session: Uuid,
        id: String,
        uuid: Uuid,
    ) -> Result<bool, Error> {
        let mut state = self.state.lock().await;
        state.account(session)?;
        if !state.cache.conversations.contains_key(&id) && !state.cache.directory_loaded {
            drop(state);
            self.refresh_directory(session).await?;
            state = self.state.lock().await;
            state.account(session)?;
        }
        let cache = &mut state.cache;
        let room = cache
            .conversations
            .get_mut(&id)
            .ok_or(Error::Conversation)?;
        let key = (id, uuid);
        if cache.subscriptions.contains_key(&key) {
            return Ok(false);
        }
        let changes = room
            .journal
            .get_or_insert_with(MessageJournal::default)
            .changed
            .subscribe();
        let (stop, cancel) = oneshot::channel();
        cache.subscriptions.insert(
            key,
            Subscription {
                registration: Uuid::new_v4(),
                pending: Some(Delivery {
                    session,
                    changes,
                    cancel,
                }),
                _cancel: stop,
            },
        );
        self.subscriptions_changed.notify_one();
        Ok(true)
    }

    pub(super) async fn unsubscribe_messages(&self, id: String, uuid: Uuid) {
        self.state.lock().await.cache.unsubscribe(&(id, uuid), None);
    }

    pub(super) async fn subscription_workers(self: &Arc<Self>, emacs: &Arc<EmacsService>) {
        let mut workers = JoinSet::new();
        loop {
            let pending: Vec<_> = self
                .state
                .lock()
                .await
                .cache
                .subscriptions
                .iter_mut()
                .filter_map(|(key, entry)| {
                    Some((key.clone(), entry.registration, entry.pending.take()?))
                })
                .collect();
            for (
                key,
                registration,
                Delivery {
                    session,
                    mut changes,
                    mut cancel,
                },
            ) in pending
            {
                let service = self.clone();
                let emacs = emacs.clone();
                workers.spawn(async move {
                    tokio::select! {
                        biased;
                        _ = &mut cancel => {},
                        _ = service.deliver_subscription(&emacs, session, &key, &mut changes) => {},
                    }
                    service
                        .state
                        .lock()
                        .await
                        .cache
                        .unsubscribe(&key, Some(registration));
                });
            }
            tokio::select! {
                _ = self.subscriptions_changed.notified() => {},
                _ = workers.join_next(), if !workers.is_empty() => {},
            }
        }
    }

    async fn deliver_subscription(
        &self,
        emacs: &EmacsService,
        session: Uuid,
        key: &SubscriptionKey,
        changes: &mut watch::Receiver<u64>,
    ) {
        let team = {
            let state = self.state.lock().await;
            let Ok(account) = state.account(session) else {
                return;
            };
            account.identity.team_id.clone()
        };
        let mut sequence = 1;
        let mut revision = None;
        loop {
            changes.borrow_and_update();
            let mut payload = json!({
                "api_version":"1", "subscription_id":key.1,
                "conversation_id":format!("{team}.{}", key.0),
                "session_id":session, "sequence":sequence
            });
            let update = match revision {
                Some(previous) => {
                    self.subscription_changes(session, &team, &key.0, previous)
                        .await
                }
                None => self
                    .subscription_snapshot(session, &team, &key.0)
                    .await
                    .map(Some),
            };
            let (next, record) = match update {
                Ok(Some(update)) => update,
                Ok(None) if revision.is_some() => {
                    tokio::select! {
                        result = changes.changed() => {
                            if result.is_err() { return; }
                            continue;
                        },
                        _ = sleep(IDLE_PROBE) => {},
                    }
                    (0, json!({"kind":"probe"}))
                }
                Ok(None) => unreachable!(),
                Err(Error::Cursor) => {
                    revision = None;
                    continue;
                }
                Err(
                    Error::Project | Error::Conversation | Error::Session | Error::Authorization,
                ) => return,
                Err(error) => {
                    payload["kind"] = json!("probe");
                    if matches!(emacs.dispatch_slack_messages(&payload).await, Ok(false)) {
                        return;
                    }
                    sleep(Duration::from_secs(
                        error.retry_after().unwrap_or(5).clamp(1, 60),
                    ))
                    .await;
                    continue;
                }
            };
            let probe = record["kind"] == "probe";
            payload
                .as_object_mut()
                .unwrap()
                .extend(record.as_object().unwrap().clone());
            let mut backoff = 1;
            loop {
                match emacs.dispatch_slack_messages(&payload).await {
                    Ok(false) => return,
                    Ok(true) => break,
                    Err(_) => {
                        sleep(Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(30);
                    }
                }
            }
            if !probe {
                revision = Some(next);
                sequence += 1;
            }
        }
    }

    async fn subscription_snapshot(
        &self,
        session: Uuid,
        team: &str,
        id: &str,
    ) -> Result<(u64, Value), Error> {
        let live = match self.refresh_latest(session, id).await {
            Ok(()) => true,
            Err(Error::Unavailable | Error::Upstream | Error::RateLimited(_)) => false,
            Err(error) => return Err(error),
        };
        let state = self.state.clone().lock_owned().await;
        let id = id.to_owned();
        let (revision, snapshot) = tokio::task::spawn_blocking(move || {
            let account = state.account(session)?;
            let room = state.cache.conversations.get(&id).ok_or(Error::Conversation)?;
            let journal = room.journal.as_ref().ok_or(Error::Conversation)?;
            let mut items: BTreeMap<_, _> = room.messages.values()
                .filter(|message| message.thread_ts.is_none())
                .map(|message| (message.ts.clone(), message.clone()))
                .collect();
            if !live && items.len() < PAGE_SIZE && let Some(disk) = &room.disk {
                for message in disk.messages(&id, None) {
                    if !room.deleted.contains(&message.ts) {
                        items.entry(message.ts.clone()).or_insert(message);
                    }
                }
            }
            if !live && items.is_empty() && !room.loaded.contains_key("") {
                return Err(Error::Unavailable);
            }
            let mut items: Vec<_> = items.into_values().collect();
            items.sort_unstable_by_key(|message| timestamp(&message.ts));
            let more = items.len() > PAGE_SIZE || room.loaded.get("").copied().unwrap_or(true);
            let items = &items[items.len().saturating_sub(PAGE_SIZE)..];
            let cursor = items.first().filter(|_| more)
                .map(|first| next_cursor(session, format!("messages:{id}:"), first.ts.clone()));
            let range = items.first().zip(items.last()).filter(|_| live)
                .map(|(first, last)| json!({"from_ts":first.ts,"through_ts":last.ts}));
            let snapshot = json!({
                "api_version":"1", "conversation_id":id,
                "items": items.iter().map(|message| room.message_result(message)).collect::<Vec<_>>(),
                "next_cursor":cursor, "sync_state":if live { "live" } else { "cached" },
                "covered_range":range, "read_through_ts":room.read_positions.get(None),
                "first_unread_ts":room.messages.values()
                    .filter(|message| message.thread_ts.is_none() && room.read_positions.unread(message, account))
                    .min_by_key(|message| timestamp(&message.ts)).map(|message| &message.ts)
            });
            Ok((*journal.changed.borrow(), snapshot))
        }).await.map_err(|_| Error::Unavailable)??;
        let snapshot = super::routes::messages_result(snapshot, team);
        Ok((revision, json!({"kind":"snapshot", "snapshot":snapshot})))
    }

    async fn subscription_changes(
        &self,
        session: Uuid,
        team: &str,
        id: &str,
        previous: u64,
    ) -> Result<Option<(u64, Value)>, Error> {
        let state = self.state.lock().await;
        state.account(session)?;
        let room = state
            .cache
            .conversations
            .get(id)
            .ok_or(Error::Conversation)?;
        let journal = room.journal.as_ref().ok_or(Error::Conversation)?;
        let revision = *journal.changed.borrow();
        if revision == previous {
            return Ok(None);
        }
        if journal.overflowed(previous) {
            return Err(Error::Cursor);
        }
        let mut effective = BTreeMap::new();
        for (_, ts, value) in journal
            .entries
            .iter()
            .filter(|(index, _, _)| *index > previous)
        {
            effective.insert(ts, value);
        }
        let (mut items, mut deleted_ts) = (Vec::new(), Vec::new());
        for (ts, value) in effective {
            if let Some(message) = value {
                items.push(room.message_result(message));
            } else {
                deleted_ts.push(ts);
            }
        }
        let update = json!({"kind":"changes", "items":items,"deleted_ts":deleted_ts});
        Ok(Some((
            revision,
            super::routes::messages_result(update, team),
        )))
    }
}
