use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message as Frame;
use uuid::Uuid;

use crate::modules::notification::protocol::NotificationRequest;

use super::{
    client::Socket,
    protocol::{Error, Message, Page, timestamp, valid_id},
    service::SlackService,
    store::{Conversation, SendState},
};

impl SlackService {
    pub async fn run(self: Arc<Self>) {
        self.restore_user_token().await;
        self.publish_status().await;
        self.sync.notify_one();
        tokio::join!(
            self.sockets(),
            self.sending(),
            self.synchronizing(),
            self.warming_users(),
            self.refreshing_picker(),
            self.notifications()
        );
    }

    async fn restore_user_token(&self) {
        if let Err(error) = self.restore_imported_token().await {
            self.state.lock().await.issue = Some(error);
        }
    }

    async fn warming_users(&self) {
        let mut changes = self.change.subscribe();
        loop {
            let delay = match self.session_id().await {
                Some(session) => match self.users(session).await {
                    Ok(_) => 300,
                    Err(error) => {
                        self.picker_failure(session).await;
                        error.retry_after().unwrap_or(60)
                    }
                },
                None => 60,
            };
            tokio::select! {
                _ = changes.changed() => {},
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
            }
        }
    }

    async fn notifications(&self) {
        loop {
            let pending = {
                let state = self.state.lock().await;
                state
                    .cache
                    .session_id
                    .filter(|session| state.account(*session).is_ok())
                    .and_then(|session| {
                        state
                            .cache
                            .notifications
                            .first_key_value()
                            .map(|(id, callback)| (session, id.clone(), callback.clone()))
                    })
            };
            let Some((session, id, callback)) = pending else {
                self.alerts.notified().await;
                continue;
            };
            let delivered = self
                .notification
                .send(NotificationRequest {
                    title: "Slack 新消息".into(),
                    body: None,
                    app: Some("slack".into()),
                    callback_url: Some(callback),
                })
                .await;
            if delivered.is_ok() {
                let removed = self
                    .update_cache(session, move |cache, _| {
                        cache.notifications.remove(&id);
                        Ok(())
                    })
                    .await;
                if removed.is_ok() {
                    continue;
                }
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    }

    async fn sockets(&self) {
        let mut changes = self.change.subscribe();
        let mut backoff = 1;
        loop {
            let Some(session) = self.session_id().await else {
                self.state.lock().await.socket_state = "stopped";
                self.publish_status().await;
                tokio::select! {
                    _ = changes.changed() => { backoff = 1; }
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {
                        self.restore_user_token().await;
                        self.publish_status().await;
                    }
                }
                continue;
            };
            self.state.lock().await.socket_state = if backoff == 1 {
                "connecting"
            } else {
                "reconnecting"
            };
            self.publish_status().await;
            let connection = async {
                let web = self.web_session(session).await?;
                let token = self.state.lock().await.account(session)?.access.clone();
                let socket = self.client.connect(&token, web.as_ref()).await?;
                self.socket_loop(session, socket).await
            };
            let started = tokio::time::Instant::now();
            let outcome = tokio::select! {
                _ = changes.changed() => { backoff = 1; continue; }
                result = connection => result,
            };
            let mut state = self.state.lock().await;
            state.socket_state = "reconnecting";
            state.issue = outcome.err();
            let retry = state.issue.and_then(Error::retry_after).unwrap_or(backoff);
            drop(state);
            self.publish_status().await;
            if outcome.is_ok() || started.elapsed() > Duration::from_secs(30) {
                backoff = 1;
            }
            let jitter = u64::from(Uuid::new_v4().as_bytes()[0]) * 3;
            tokio::select! {
                _ = changes.changed() => { backoff = 1; }
                _ = tokio::time::sleep(Duration::from_secs(retry) + Duration::from_millis(jitter)) => {
                    backoff = (backoff * 2).min(60);
                }
            }
        }
    }

    async fn socket_loop(&self, session: Uuid, mut socket: Socket) -> Result<(), Error> {
        loop {
            let incoming = match tokio::time::timeout(Duration::from_secs(30), socket.next()).await
            {
                Ok(incoming) => incoming,
                Err(_) => {
                    socket
                        .send(Frame::Ping(Vec::new().into()))
                        .await
                        .map_err(|_| Error::Unavailable)?;
                    tokio::time::timeout(Duration::from_secs(15), socket.next())
                        .await
                        .map_err(|_| Error::Unavailable)?
                }
            };
            let incoming = incoming
                .ok_or(Error::Unavailable)?
                .map_err(|_| Error::Unavailable)?;
            let content = match incoming {
                Frame::Text(content) => content,
                Frame::Ping(payload) => {
                    socket
                        .send(Frame::Pong(payload))
                        .await
                        .map_err(|_| Error::Unavailable)?;
                    continue;
                }
                Frame::Close(_) => return Err(Error::Unavailable),
                _ => continue,
            };
            let event: Value = serde_json::from_str(&content).map_err(|_| Error::Upstream)?;
            match event["type"].as_str() {
                Some("hello") => {
                    let mut state = self.state.lock().await;
                    state.account(session)?;
                    state.socket_state = "connected";
                    state.issue = None;
                    drop(state);
                    self.publish_status().await;
                    self.sync.notify_one();
                    self.outgoing.notify_one();
                }
                Some("disconnect" | "goodbye") => return Ok(()),
                Some("message") => {
                    if self.accept_message(session, &event).await? {
                        self.alerts.notify_one();
                    }
                }
                Some("user_change" | "team_join") => {
                    let user = &event["user"];
                    if let Some(id) = user["id"].as_str() {
                        self.update_directory_user(session, id, user).await;
                    }
                }
                _ => {}
            }
        }
    }

    async fn accept_message(&self, session: Uuid, event: &Value) -> Result<bool, Error> {
        let Some(id) = event["channel"].as_str().filter(|id| valid_id(id)) else {
            return Ok(false);
        };
        let id = id.to_owned();
        let team = event["team"].as_str().map(str::to_owned);
        let channel_type = event["channel_type"].as_str().map(str::to_owned);
        let subtype = event["subtype"].as_str().unwrap_or_default().to_owned();
        let message = Message::from_slack(
            if subtype == "message_changed" {
                &event["message"]
            } else {
                event
            },
            None,
        );
        let deleted = event["deleted_ts"]
            .as_str()
            .filter(|ts| timestamp(ts).is_some())
            .map(str::to_owned);
        let new = subtype.is_empty()
            || matches!(
                subtype.as_str(),
                "bot_message" | "file_share" | "thread_broadcast"
            );
        let event_ts = event["event_ts"]
            .as_str()
            .or_else(|| event["message"]["edited"]["ts"].as_str())
            .or_else(|| event["ts"].as_str())
            .or_else(|| deleted.as_deref())
            .or_else(|| message.as_ref().map(|message| message.ts.as_str()));
        let Some(event_ts) = event_ts.filter(|ts| timestamp(ts).is_some()) else {
            return Ok(false);
        };
        let event_id = format!("{id}.{subtype}.{event_ts}");
        self.update_cache(session, move |cache, account| {
            if team
                .as_deref()
                .is_some_and(|team| team != account.identity.team_id)
            {
                return Ok(false);
            }
            let kind = match cache.conversations.get(&id) {
                Some(room) => room.kind.as_str(),
                None if id.starts_with('D') || channel_type.as_deref() == Some("im") => "im",
                None if channel_type.as_deref() == Some("mpim") => "mpim",
                None => return Ok(false),
            }
            .to_owned();
            let private = matches!(kind.as_str(), "im" | "mpim");
            if !cache.events.insert(event_id.clone()) {
                return Ok(false);
            }
            if cache.events.len() > 10_000 {
                cache.events.pop_first();
            }
            let unread = private
                && new
                && message
                    .as_ref()
                    .is_some_and(|message| cache.positions(&id).unread(message, account));
            let conversation = cache
                .conversations
                .entry(id.clone())
                .or_insert_with(|| Conversation::new(&id, &kind, &id));
            if subtype == "message_deleted" {
                if let Some(deleted) = deleted {
                    conversation.remove(&deleted);
                }
                return Ok(false);
            }
            let Some(message) = message else {
                return Ok(false);
            };
            let notify = unread && !conversation.deleted.contains(&message.ts);
            if let Some(root) = &message.thread_ts {
                let count = conversation
                    .messages
                    .values()
                    .filter(|item| item.thread_ts.as_ref() == Some(root))
                    .count()
                    + usize::from(!conversation.messages.contains_key(&message.ts));
                if let Some(parent) = conversation.messages.get_mut(root) {
                    parent.reply_count = parent.reply_count.max(count as u64);
                }
            }
            if notify {
                let mut callback = format!(
                    "/slack/conversations/{}.{id}/open?message_ts={}",
                    account.identity.team_id, message.ts
                );
                if let Some(thread) = &message.thread_ts {
                    callback.push_str("&thread_ts=");
                    callback.push_str(thread);
                }
                cache.notifications.insert(event_id, callback);
                if cache.notifications.len() > 512 {
                    cache.notifications.pop_first();
                }
            }
            conversation.ingest(message);
            Ok(notify)
        })
        .await
    }

    async fn synchronizing(&self) {
        let mut changes = self.change.subscribe();
        loop {
            let reconnect = tokio::select! {
                _ = self.sync.notified() => true,
                _ = changes.changed() => true,
                _ = tokio::time::sleep(Duration::from_secs(120)) => false,
            };
            let Some(session) = self.session_id().await else {
                continue;
            };
            if self.refresh_directory(session).await.is_err() {
                self.picker_failure(session).await;
                continue;
            }
            self.refresh_recent_messages(session, reconnect).await;
            let _ = self.refresh_group_members(session).await;
            self.refresh_names(session).await;
        }
    }

    async fn refresh_recent_messages(&self, session: Uuid, reconnect: bool) {
        let rooms: Vec<_> = {
            let state = self.state.lock().await;
            if state.account(session).is_err() {
                return;
            }
            state
                .cache
                .conversations
                .values()
                .filter(|item| reconnect || !item.loaded.contains_key(""))
                .map(|item| {
                    let checkpoint = item
                        .messages
                        .values()
                        .filter(|message| message.thread_ts.is_none())
                        .max_by_key(|message| timestamp(&message.ts))
                        .map(|message| message.ts.clone());
                    let threads = if reconnect {
                        item.loaded
                            .keys()
                            .rev()
                            .filter(|ts| !ts.is_empty())
                            .take(5)
                            .cloned()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    (item.id.clone(), checkpoint, threads)
                })
                .collect()
        };
        for (id, checkpoint, threads) in rooms {
            let result = if let Some(checkpoint) = checkpoint {
                self.catch_up(session, &id, None, Some(&checkpoint)).await
            } else {
                self.refresh_latest(session, &id).await
            };
            if let Err(error) = result {
                self.picker_failure(session).await;
                if matches!(
                    error,
                    Error::RateLimited(_)
                        | Error::Session
                        | Error::Authorization
                        | Error::Unavailable
                ) {
                    break;
                }
            }
            for thread in threads {
                let _ = self.catch_up(session, &id, Some(thread), None).await;
            }
        }
    }

    async fn catch_up(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<String>,
        checkpoint: Option<&str>,
    ) -> Result<(), Error> {
        let mut cursor = None;
        loop {
            let page = self
                .messages(
                    session,
                    id.to_owned(),
                    thread.clone(),
                    Page {
                        limit: Some(100),
                        cursor: cursor.clone(),
                        ..Page::default()
                    },
                )
                .await?;
            if page["sync_state"] != "live"
                || checkpoint.is_none()
                || page["items"].as_array().is_none_or(|items| {
                    items.first().is_none_or(|item| {
                        item["ts"].as_str().and_then(timestamp) <= checkpoint.and_then(timestamp)
                    })
                })
            {
                break;
            }
            let next = page["next_cursor"].as_str().map(str::to_owned);
            if next.is_none() || next == cursor {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    async fn refresh_names(&self, session: Uuid) {
        let ids = {
            let state = self.state.lock().await;
            if state.account(session).is_err() {
                return;
            }
            state
                .cache
                .conversations
                .values()
                .filter(|item| matches!(item.kind.as_str(), "im" | "mpim"))
                .flat_map(|item| item.participants.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>()
        };
        if ids.is_empty() {
            return;
        }
        let mut names: BTreeMap<_, _> = self
            .users(session)
            .await
            .ok()
            .iter()
            .flat_map(|users| users.iter())
            .map(|user| (user.slack_id().to_owned(), user.name.clone()))
            .collect();
        for id in ids {
            if names.get(&id).is_some_and(|name| name != &id) {
                continue;
            }
            match self
                .slack_get(session, "users.info", &[("user", &id)])
                .await
            {
                Ok(value) => {
                    if let Some(name) = value["user"]["profile"]["display_name"]
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .or(value["user"]["real_name"].as_str())
                        .or(value["user"]["name"].as_str())
                    {
                        self.update_directory_user(session, &id, &value["user"])
                            .await;
                        names.insert(id, name.to_owned());
                    }
                }
                Err(
                    Error::Permission
                    | Error::RateLimited(_)
                    | Error::Authorization
                    | Error::Session,
                ) => break,
                Err(_) => {}
            }
        }
        if names.is_empty() {
            return;
        }
        let _ = self
            .update_cache(session, move |cache, account| {
                for conversation in cache.conversations.values_mut() {
                    let labels: Vec<_> = conversation
                        .participants
                        .iter()
                        .filter(|id| *id != &account.identity.user_id)
                        .filter_map(|id| names.get(id).cloned())
                        .collect();
                    if !labels.is_empty() && matches!(conversation.kind.as_str(), "im" | "mpim") {
                        conversation.title = labels.join("、");
                    }
                    for message in conversation.messages.values_mut() {
                        if let Some(author) = &mut message.author
                            && let Some(name) = names.get(&author.id)
                        {
                            author.name.clone_from(name);
                        }
                    }
                }
                Ok(())
            })
            .await;
    }

    async fn sending(&self) {
        let mut changes = self.change.subscribe();
        loop {
            let work = {
                let state = self.state.lock().await;
                state
                    .secrets
                    .account
                    .as_ref()
                    .filter(|account| state.account(account.identity.session_id).is_ok())
                    .and_then(|account| {
                        state
                            .cache
                            .sends
                            .values()
                            .find(|record| record.state == SendState::Pending)
                            .map(|record| (account.identity.session_id, record.id))
                    })
            };
            if let Some((session, id)) = work {
                self.send_one(session, id, &mut changes).await;
            } else {
                tokio::select! {
                    _ = self.outgoing.notified() => {}
                    _ = changes.changed() => {}
                }
            }
        }
    }

    async fn send_one(&self, session: Uuid, id: Uuid, changes: &mut watch::Receiver<u64>) {
        let _ = changes.borrow_and_update();
        let record = self
            .update_cache(session, move |cache, _| {
                let record = cache.sends.get_mut(&id).ok_or(Error::SendRecord)?;
                if record.state != SendState::Pending {
                    return Err(Error::Conflict);
                }
                record.state = SendState::Attempting;
                Ok(record.clone())
            })
            .await;
        let Ok(record) = record else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            return;
        };
        let outcome = {
            let mut body = json!({"channel":record.conversation, "text":record.text,
                    "mrkdwn":false,"unfurl_links":false,"unfurl_media":false});
            if let Some(thread) = &record.thread {
                body["thread_ts"] = json!(thread);
            }
            tokio::select! {
                _ = changes.changed() => Err(Error::DeliveryUnknown),
                result = self.slack_post(session, "chat.postMessage", &body) => {
                    match result {
                        Ok(value) => value["ts"].as_str().filter(|ts| timestamp(ts).is_some()).map(|ts| {
                            let message = Message::from_slack(&value["message"], record.thread.as_deref());
                            (ts.to_owned(), message)
                        }).ok_or(Error::DeliveryUnknown),
                        Err(Error::Upstream | Error::Unavailable) => Err(Error::DeliveryUnknown),
                        Err(error) => Err(error)
                    }
                }
            }
        };
        let _ = self
            .update_cache(session, move |cache, _| {
                let record = cache.sends.get_mut(&id).ok_or(Error::SendRecord)?;
                match outcome {
                    Ok((ts, message)) => {
                        record.state = SendState::Sent;
                        record.ts = Some(ts);
                        if let Some(message) = message
                            && let Some(conversation) =
                                cache.conversations.get_mut(&record.conversation)
                        {
                            conversation.ingest(message);
                        }
                    }
                    Err(error) => {
                        record.state = if error == Error::DeliveryUnknown {
                            SendState::Unknown
                        } else {
                            SendState::Failed
                        };
                        record.failure = Some(error);
                    }
                }
                record.text = None;
                Ok(())
            })
            .await;
    }
}
