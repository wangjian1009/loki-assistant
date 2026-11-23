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
    protocol::{Error, Message, Page, timestamp},
    service::SlackService,
    store::Cache,
};

const JOURNAL_LIMIT: usize = 512;
const PAGE_SIZE: usize = 50;
const IDLE_PROBE: Duration = Duration::from_secs(60);

pub(super) type SubscriptionKey = (String, Uuid);

pub(super) fn thread_roots(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter(|item| item["reply_count"].as_u64().unwrap_or_default() > 0)
        .filter_map(|item| item["ts"].as_str().map(str::to_owned))
        .collect()
}

#[derive(Default)]
pub(super) struct MessageJournal {
    changed: watch::Sender<u64>,
    entries: VecDeque<MessageChange>,
    nickname_revision: Option<u64>,
    profiles: VecDeque<(u64, BTreeSet<String>)>,
    profiles_dropped_through: u64,
    dropped_through: u64,
}

struct MessageChange {
    revision: u64,
    ts: String,
    thread_ts: Option<String>,
    message: Option<Message>,
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
                .filter(|entry| entry.revision > *revision)
                .map(|entry| entry.ts.clone())
                .collect(),
        )
    }

    fn overflowed(&self, revision: u64) -> bool {
        self.dropped_through > revision
    }

    pub fn nicknames_changed(&mut self) {
        let revision = *self.changed.borrow() + 1;
        self.nickname_revision = Some(revision);
        self.changed.send_replace(revision);
    }

    pub fn profiles_changed(&mut self, ids: &BTreeSet<String>) {
        let revision = *self.changed.borrow() + 1;
        self.profiles.push_back((revision, ids.clone()));
        if self.profiles.len() > JOURNAL_LIMIT
            && let Some((revision, _)) = self.profiles.pop_front()
        {
            self.profiles_dropped_through = revision;
        }
        self.changed.send_replace(revision);
    }

    pub fn push(&mut self, ts: String, thread_ts: Option<String>, message: Option<Message>) {
        let revision = *self.changed.borrow() + 1;
        self.entries.push_back(MessageChange {
            revision,
            ts,
            thread_ts,
            message,
        });
        if self.entries.len() > JOURNAL_LIMIT
            && let Some(oldest) = self.entries.pop_front()
        {
            self.dropped_through = oldest.revision;
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
    pub(super) async fn has_message_subscribers(&self, id: &str) -> bool {
        let id = id.to_owned();
        self.state
            .call(move |state| {
                state
                    .cache
                    .conversations
                    .get(&id)
                    .is_some_and(|room| room.journal.is_some())
            })
            .await
    }

    pub(super) async fn refresh_threads(&self, session: Uuid, id: &str, roots: Vec<String>) {
        for root in roots {
            let result = self
                .messages(session, id.to_owned(), Some(root), Page::default())
                .await;
            if matches!(
                result,
                Err(Error::Session | Error::Authorization | Error::RateLimited(_))
            ) {
                break;
            }
        }
    }

    pub(super) async fn subscribe_messages(
        &self,
        session: Uuid,
        id: String,
        uuid: Uuid,
    ) -> Result<bool, Error> {
        let room_id = id.clone();
        let refresh = self
            .state
            .call(move |state| {
                state.account(session)?;
                Ok(!state.cache.conversations.contains_key(&room_id)
                    && !state.cache.directory_loaded)
            })
            .await?;
        if refresh {
            self.refresh_directory(session).await?;
        }
        let created = self
            .state
            .call(move |state| {
                state.account(session)?;
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
                Ok(true)
            })
            .await?;
        if created {
            self.subscriptions_changed.notify_one();
        }
        Ok(created)
    }

    pub(super) async fn unsubscribe_messages(&self, id: String, uuid: Uuid) {
        self.state
            .call(move |state| state.cache.unsubscribe(&(id, uuid), None))
            .await;
    }

    pub(super) async fn subscription_workers(self: &Arc<Self>, emacs: &Arc<EmacsService>) {
        let mut workers = JoinSet::new();
        loop {
            let pending: Vec<_> = self
                .state
                .call(|state| {
                    state
                        .cache
                        .subscriptions
                        .iter_mut()
                        .filter_map(|(key, entry)| {
                            Some((key.clone(), entry.registration, entry.pending.take()?))
                        })
                        .collect()
                })
                .await;
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
                        .call(move |state| state.cache.unsubscribe(&key, Some(registration)))
                        .await;
                });
            }
            tokio::select! {
                _ = self.subscriptions_changed.notified() => {},
                _ = workers.join_next(), if !workers.is_empty() => {},
            }
        }
    }

    async fn deliver_subscription(
        self: &Arc<Self>,
        emacs: &EmacsService,
        session: Uuid,
        key: &SubscriptionKey,
        changes: &mut watch::Receiver<u64>,
    ) {
        let Some(team) = self
            .state
            .call(move |state| {
                state
                    .account(session)
                    .ok()
                    .map(|account| account.identity.team_id.clone())
            })
            .await
        else {
            return;
        };
        let mut sequence = 1;
        let mut revision = None;
        let mut thread_loads = JoinSet::new();
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
                        _ = thread_loads.join_next(), if !thread_loads.is_empty() => continue,
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
                if record["kind"] == "snapshot" {
                    let roots = thread_roots(&record["snapshot"]);
                    thread_loads.abort_all();
                    if !roots.is_empty() {
                        let service = Arc::clone(self);
                        let id = key.0.clone();
                        thread_loads.spawn(async move {
                            service.refresh_threads(session, &id, roots).await;
                        });
                    }
                }
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
        let id = id.to_owned();
        let (revision, snapshot) = self.state.blocking(move |state| {
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
            let replies = room.cached_replies(items, true, account, state.user_directory.as_ref());
            let snapshot = json!({
                "api_version":"1", "conversation_id":id,
                "items": items.iter().map(|message| room.message_result(message, account, state.user_directory.as_ref())).collect::<Vec<_>>(),
                "thread_items": replies,
                "nicknames": account.team_nicknames(),
                "title": room.display_title(account, state.user_directory.as_ref()),
                "next_cursor":cursor, "sync_state":if live { "live" } else { "cached" },
                "covered_range":range, "read_through_ts":room.read_positions.get(None),
                "first_unread_ts":room.messages.values()
                    .filter(|message| message.thread_ts.is_none() && room.read_positions.unread(message, account))
                    .min_by_key(|message| timestamp(&message.ts)).map(|message| &message.ts)
            });
            Ok((*journal.changed.borrow(), snapshot))
        }).await?;
        let nicknames = self.team_nicknames(team).await;
        let snapshot = super::routes::messages_result(snapshot, team, &nicknames);
        Ok((revision, json!({"kind":"snapshot", "snapshot":snapshot})))
    }

    async fn subscription_changes(
        &self,
        session: Uuid,
        team: &str,
        id: &str,
        previous: u64,
    ) -> Result<Option<(u64, Value)>, Error> {
        let (id, team) = (id.to_owned(), team.to_owned());
        let names = self.team_nicknames(&team).await;
        self.state.call(move |state| {
        let account = state.account(session)?;
        let room = state
            .cache
            .conversations
            .get(&id)
            .ok_or(Error::Conversation)?;
        let journal = room.journal.as_ref().ok_or(Error::Conversation)?;
        let revision = *journal.changed.borrow();
        if revision == previous {
            return Ok(None);
        }
        if journal.overflowed(previous) || journal.profiles_dropped_through > previous {
            return Err(Error::Cursor);
        }
        let mut effective = BTreeMap::new();
        for entry in journal
            .entries
            .iter()
            .filter(|entry| entry.revision > previous)
        {
            effective.insert(&entry.ts, entry);
        }
        let (mut items, mut deleted_ts) = (Vec::new(), Vec::new());
        let (mut thread_items, mut deleted_thread_messages) = (Vec::new(), Vec::new());
        for (&ts, entry) in &effective {
            match (&entry.thread_ts, &entry.message) {
                (None, Some(message)) => items.push(room.message_result(message, account, state.user_directory.as_ref())),
                (None, None) => deleted_ts.push(ts),
                (Some(_), Some(message)) => thread_items.push(room.message_result(message, account, state.user_directory.as_ref())),
                (Some(root), None) => {
                    deleted_thread_messages.push(json!({"thread_ts":root, "ts":ts}))
                }
            }
        }
        let changed_profiles: BTreeSet<_> = journal.profiles.iter()
            .filter(|(revision, _)| *revision > previous)
            .flat_map(|(_, ids)| ids.iter().map(String::as_str))
            .collect();
        if !changed_profiles.is_empty() {
            for message in room.messages.values().filter(|message| {
                !effective.contains_key(&message.ts) && (
                    message.author.as_ref().is_some_and(|author| changed_profiles.contains(author.id.as_str()))
                    || message.user_references().any(|(id, _)| changed_profiles.contains(id))
                )
            }) {
                let value = room.message_result(message, account, state.user_directory.as_ref());
                if message.thread_ts.is_some() {
                    thread_items.push(value);
                } else {
                    items.push(value);
                }
            }
        }
        let mut update = json!({"kind":"changes", "items":items, "deleted_ts":deleted_ts,
                            "thread_items":thread_items, "deleted_thread_messages":deleted_thread_messages});
        if journal.nickname_revision.is_some_and(|revision| revision > previous) {
            update["nicknames"] = json!(account.team_nicknames());
        }
        if !changed_profiles.is_empty() || journal.nickname_revision.is_some_and(|revision| revision > previous) {
            update["title"] = json!(room.display_title(account, state.user_directory.as_ref()));
        }
        Ok(Some((
            revision,
            super::routes::messages_result(update, &team, &names),
        )))
        }).await
    }
}
