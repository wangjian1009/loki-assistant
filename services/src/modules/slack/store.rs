use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use uuid::Uuid;

use super::disk::DiskCache;
use super::protocol::{Error, Identity, Message, timestamp};
use super::subscriptions::{MessageJournal, Subscription, SubscriptionKey};

#[derive(Default)]
pub(super) struct Secrets {
    pub account: Option<Account>,
    pub disabled_token: Option<String>,
}

pub(super) struct Account {
    pub identity: Identity,
    pub access: String,
    pub web_session: bool,
    pub baseline: u64,
    pub imported_token_hash: Option<String>,
    pub revoked: bool,
}

#[derive(Default)]
pub(super) struct Cache {
    pub disk: Option<DiskCache>,
    pub session_id: Option<Uuid>,
    pub conversations: BTreeMap<String, Conversation>,
    pub directory_loaded: bool,
    pub sends: BTreeMap<Uuid, SendRecord>,
    pub events: BTreeSet<String>,
    pub new_messages: BTreeSet<((u64, u32), String)>,
    pub notifications: BTreeMap<String, PendingNotification>,
    pub subscriptions: BTreeMap<SubscriptionKey, Subscription>,
}

#[derive(Clone)]
pub(super) struct PendingNotification {
    pub body: String,
    pub callback: String,
    pub watch: Option<String>,
}

impl Cache {
    pub fn reset(&mut self, session_id: Option<Uuid>) {
        if self.session_id != session_id {
            *self = Self {
                session_id,
                ..Self::default()
            };
        }
    }
}

pub(super) struct Conversation {
    pub disk: Option<DiskCache>,
    pub id: String,
    pub kind: String,
    pub title: String,
    pub participants: Vec<String>,
    pub read_positions: ReadPositions,
    pub messages: BTreeMap<String, Message>,
    pub deleted: BTreeSet<String>,
    pub loaded: BTreeMap<String, bool>,
    pub journal: Option<MessageJournal>,
}

#[derive(Clone, Default)]
pub(super) struct ReadPositions {
    pub baseline: Option<u64>,
    pub cursors: BTreeMap<String, String>,
}

impl Conversation {
    pub fn message_result(&self, message: &Message) -> Value {
        let mut value = json!(message);
        if let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) {
            for (value, item) in attachments.iter_mut().zip(&message.attachments) {
                value["original_available"] = json!(item.download_url.is_some());
            }
        }
        if let Some(disk) = &self.disk {
            return disk.message_result(&self.id, message, value);
        }
        value
    }

    pub fn new(id: &str, kind: &str, title: &str) -> Self {
        Self {
            disk: None,
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            participants: Vec::new(),
            read_positions: ReadPositions::default(),
            messages: BTreeMap::new(),
            deleted: BTreeSet::new(),
            loaded: BTreeMap::new(),
            journal: None,
        }
    }

    pub fn cached_replies(&self, roots: &[Message], include_other: bool) -> Vec<Value> {
        let mut replies = BTreeMap::new();
        if let Some(disk) = &self.disk {
            for root in roots.iter().filter(|root| root.reply_count > 0) {
                let mut items: Vec<_> = disk
                    .messages(&self.id, Some(&root.ts))
                    .filter(|message| !self.deleted.contains(&message.ts))
                    .collect();
                items.sort_unstable_by_key(|message| timestamp(&message.ts));
                for message in items.into_iter().rev().take(50) {
                    replies.insert(message.ts.clone(), self.message_result(&message));
                }
            }
        }
        for message in self.messages.values() {
            if let Some(thread) = &message.thread_ts
                && (include_other || roots.iter().any(|root| &root.ts == thread))
            {
                replies.insert(message.ts.clone(), self.message_result(message));
            }
        }
        replies.into_values().collect()
    }

    pub fn summarize(&self, account: &Account) -> (Option<(u64, u32)>, Option<(u64, u32)>, Value) {
        let latest = self
            .messages
            .values()
            .max_by_key(|message| timestamp(&message.ts));
        let mut unread: Vec<_> = self
            .messages
            .values()
            .filter(|message| self.read_positions.unread(message, account))
            .collect();
        unread.sort_by_key(|message| timestamp(&message.ts));
        let mention = unread
            .iter()
            .rev()
            .find(|message| message.mentions(&account.identity.user_id));
        let participants: Vec<_> = self
            .participants
            .iter()
            .filter(|id| *id != &account.identity.user_id)
            .map(|id| format!("{}.{id}", account.identity.team_id))
            .collect();
        (
            latest.and_then(|message| timestamp(&message.ts)),
            unread.last().and_then(|message| timestamp(&message.ts)),
            json!({
                "id":self.id, "kind":self.kind, "title":self.title,
                "participant_ids":participants,
                "last_activity_ts":latest.map(|message| &message.ts),
                "history_loaded":self.loaded.contains_key(""),
                "preview":latest.map(Message::preview),
                "last_message":latest.map(|message| json!({"author":message.author.as_ref().map(|author| json!({
                    "id":format!("{}.{}", account.identity.team_id, author.id),"name":author.name})),
                    "ts":message.ts,"preview":message.preview()})),
                "unread_count":unread.len(),
                "last_unread_mention_ts":mention.map(|message| &message.ts),
                "first_unread":unread.first().map(|message| json!({"ts":message.ts, "thread_ts":message.thread_ts}))
            }),
        )
    }

    pub fn ingest(&mut self, message: Message) {
        if self.deleted.contains(&message.ts) {
            return;
        }
        if self
            .messages
            .get(&message.ts)
            .is_some_and(|old| old == &message || (old.edited && !message.edited))
        {
            return;
        }
        if let Some(disk) = &self.disk {
            let _ = disk.save_room(self);
            disk.save_message(&self.id, &message);
        }
        if let Some(journal) = &mut self.journal {
            journal.push(
                message.ts.clone(),
                message.thread_ts.clone(),
                Some(message.clone()),
            );
        }
        self.messages.insert(message.ts.clone(), message);
        if self.messages.len() > 1_000 {
            if let Some(oldest) = self
                .messages
                .keys()
                .min_by_key(|value| timestamp(value))
                .cloned()
            {
                self.messages.remove(&oldest);
            }
        }
    }

    pub fn remove(&mut self, ts: &str, thread: Option<&str>) {
        if !self.deleted.insert(ts.into()) {
            return;
        }
        let removed = self.messages.remove(ts);
        let thread = removed
            .and_then(|message| message.thread_ts)
            .or_else(|| thread.map(str::to_owned))
            .or_else(|| self.disk.as_ref()?.find_message(&self.id, ts)?.thread_ts);
        if let Some(disk) = &self.disk {
            disk.remove_message(&self.id, ts, thread.as_deref());
        }
        if let Some(journal) = &mut self.journal {
            journal.push(ts.to_owned(), thread, None);
        }
        if self.deleted.len() > 2_000 {
            self.deleted.pop_first();
        }
    }

    pub fn update_reply_count(&mut self, ts: &str, count: u64) {
        if let Some(parent) = self.messages.get(ts) && parent.reply_count < count {
            let mut parent = parent.clone();
            parent.reply_count = count;
            self.ingest(parent);
        }
    }
}

impl ReadPositions {
    pub fn get(&self, thread: Option<&str>) -> Option<&str> {
        self.cursors
            .get(thread.unwrap_or_default())
            .map(String::as_str)
    }

    pub fn unread(&self, message: &Message, account: &Account) -> bool {
        if message
            .author
            .as_ref()
            .is_some_and(|author| author.id == account.identity.user_id)
        {
            return false;
        }
        let position = self
            .get(message.thread_ts.as_deref())
            .and_then(timestamp)
            .unwrap_or((self.baseline.unwrap_or(account.baseline), 0));
        timestamp(&message.ts).is_some_and(|value| value > position)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SendState {
    Pending,
    Attempting,
    Sent,
    Failed,
    Unknown,
}

#[derive(Clone)]
pub(super) struct SendRecord {
    pub id: Uuid,
    pub conversation: String,
    pub thread: Option<String>,
    pub fingerprint: String,
    pub text: Option<String>,
    pub state: SendState,
    pub ts: Option<String>,
    pub failure: Option<Error>,
}

impl SendRecord {
    pub fn pending(&self) -> bool {
        matches!(self.state, SendState::Pending | SendState::Attempting)
    }

    pub fn json(&self) -> Value {
        let state = if self.pending() {
            SendState::Pending
        } else {
            self.state
        };
        json!({
            "api_version":"1", "request_id":self.id, "conversation_id":self.conversation,
            "thread_ts":self.thread, "state":state,
            "result":self.ts.as_ref().map(|ts| json!({"ts":ts})),
            "error":self.failure.map(Error::object),
            "retry_after_seconds":self.failure.and_then(Error::retry_after)
        })
    }
}
