use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message as Frame;
use uuid::Uuid;

use crate::modules::{
    emacs::service::EmacsService, notification::protocol::NotificationRequest,
};

use super::{
    client::Socket,
    notification,
    protocol::{Author, Error, Message, Page, timestamp, valid_id},
    service::SlackService,
    store::{Conversation, PendingNotification, SendState},
};

impl SlackService {
    pub async fn run(self: Arc<Self>, emacs: Arc<EmacsService>) {
        self.restore_user_token().await;
        self.sync.notify_one();
        tokio::join!(
            self.sockets(),
            self.sending(),
            self.synchronizing(),
            self.warming_users(),
            self.warming_avatars(),
            self.refreshing_picker(),
            self.notifications(),
            self.subscription_workers(&emacs)
        );
    }

    async fn restore_user_token(&self) {
        let issue = self.restore_imported_token().await.err();
        self.state.lock().await.issue = issue;
        self.publish_status().await;
        self.sync_reauthorization_alert().await;
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

    async fn warming_avatars(&self) {
        let mut changes = self.change.subscribe();
        loop {
            if let Some(session) = self.session_id().await {
                tokio::select! {
                    _ = self.warm_avatars(session) => {},
                    _ = changes.changed() => continue,
                }
            }
            tokio::select! {
                _ = self.avatars_changed.notified() => {},
                _ = changes.changed() => {},
                _ = tokio::time::sleep(Duration::from_secs(300)) => {},
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
                            .map(|(id, alert)| (session, id.clone(), alert.clone()))
                    })
            };
            let Some((session, id, alert)) = pending else {
                self.alerts.notified().await;
                continue;
            };
            if let Some(watch) = &alert.watch {
                let watched = self.watching(watch).await;
                let mut state = self.state.lock().await;
                if state.account(session).is_err() || !state.cache.notifications.contains_key(&id) {
                    continue;
                }
                if !watched {
                    state.cache.notifications.remove(&id);
                    continue;
                }
            }
            let delivered = self
                .notification
                .send(NotificationRequest {
                    title: "Slack 新消息".into(),
                    body: Some(alert.body),
                    app: Some("slack".into()),
                    callback_url: Some(alert.callback),
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
                let socket = self.client.connect(&token, web.as_ref()).await;
                if matches!(socket, Err(Error::Authorization)) {
                    self.revoke(session, &token).await;
                }
                let socket = socket?;
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
        let broadcast = matches!(subtype.as_str(), "thread_broadcast" | "reply_broadcast");
        let prior_thread = event["previous_message"]["thread_ts"]
            .as_str()
            .or_else(|| broadcast.then(|| event["root"]["ts"].as_str()).flatten())
            .filter(|ts| timestamp(ts).is_some())
            .map(str::to_owned);
        let message = Message::from_slack(
            if subtype == "message_changed" {
                &event["message"]
            } else {
                event
            },
            prior_thread.as_deref(),
        );
        let deleted = event["deleted_ts"]
            .as_str()
            .filter(|ts| timestamp(ts).is_some())
            .map(str::to_owned);
        let new = subtype.is_empty()
            || broadcast
            || matches!(subtype.as_str(), "bot_message" | "file_share");
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
        let (conversation_id, users) = {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            if team.as_deref().is_some_and(|team| team != account.identity.team_id)
                || state.cache.events.contains(&event_id)
            {
                return Ok(false);
            }
            let users = state
                .user_directory
                .as_ref()
                .filter(|directory| directory.session == session)
                .cloned();
            (format!("{}.{id}", account.identity.team_id), users)
        };
        if let Some(message) = &message {
            self.warm_message_media(session, std::slice::from_ref(message)).await;
        }
        let bot = message
            .as_ref()
            .and_then(|message| message.author.as_ref())
            .is_some_and(|author| author.id.starts_with('B'));
        let _watch_change = if new {
            Some(self.watch_changes.lock().await)
        } else {
            None
        };
        let watched = new && self.watching(&conversation_id).await;
        let result = self
            .update_cache(session, move |cache, account| {
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
                    None if channel_type.as_deref() == Some("channel") => "public_channel",
                    None if channel_type.as_deref() == Some("group") => "private_channel",
                    None if watched => "public_channel",
                    None => return Ok(false),
                }
                .to_owned();
                let direct_message = matches!(kind.as_str(), "im" | "mpim");
                if !cache.events.insert(event_id.clone()) {
                    return Ok(false);
                }
                if cache.events.len() > 10_000 {
                    cache.events.pop_first();
                }
                let conversation = cache.conversations.entry(id.clone()).or_insert_with(|| {
                    let mut room = Conversation::new(&id, &kind, &id);
                    room.disk.clone_from(&cache.disk);
                    room.read_positions.baseline = Some(account.baseline);
                    room
                });
                if subtype == "message_deleted" {
                    if let Some(deleted) = deleted {
                        conversation.remove(&deleted, prior_thread.as_deref());
                    }
                    return Ok(false);
                }
                let Some(mut message) = message else {
                    return Ok(false);
                };
                if subtype == "message_changed" {
                    let previous = conversation
                        .messages
                        .get(&message.ts)
                        .cloned()
                        .or_else(|| conversation.disk.as_ref()?.find_message(&id, &message.ts));
                    if let Some(previous) = previous {
                        message.edited |= previous.edited;
                        if message.thread_ts.is_none() {
                            message.thread_ts = previous.thread_ts;
                        }
                    }
                }
                let ordinary = direct_message || message.mentions(&account.identity.user_id);
                let first_seen = timestamp(&message.ts)
                    .filter(|ts| new && cache.new_messages.insert((*ts, id.clone())));
                if cache.new_messages.len() > 10_000 {
                    cache.new_messages.pop_first();
                }
                let notification_order = first_seen.filter(|_| {
                    (ordinary || (watched && !direct_message))
                        && conversation.read_positions.unread(&message, account)
                        && !conversation.deleted.contains(&message.ts)
                });
                if let Some(root) = &message.thread_ts {
                    let count = conversation
                        .messages
                        .values()
                        .filter(|item| item.thread_ts.as_ref() == Some(root))
                        .count()
                        + usize::from(!conversation.messages.contains_key(&message.ts));
                    conversation.update_reply_count(root, count as u64);
                }
                if notification_order.is_some() {
                    let callback = notification::callback(
                        &conversation_id,
                        &message.ts,
                        message.thread_ts.as_deref(),
                    );
                    cache.notifications.insert(
                        event_id,
                        PendingNotification {
                            body: notification::body_with_nicknames(&message, users.as_ref(), Some(account)),
                            callback,
                            watch: (!ordinary).then_some(conversation_id),
                        },
                    );
                    if cache.notifications.len() > 512 {
                        cache.notifications.pop_first();
                    }
                }
                conversation.ingest(message);
                Ok(notification_order.is_some())
            })
            .await;
        if bot && result.is_ok() {
            self.avatars_changed.notify_one();
        }
        result
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
            self.avatars_changed.notify_one();
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
                        if let Some(disk) = &conversation.disk {
                            let _ = disk.save_room(conversation);
                        }
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
                body["reply_broadcast"] = json!(false);
            }
            tokio::select! {
                _ = changes.changed() => Err(Error::DeliveryUnknown),
                result = self.slack_post(session, "chat.postMessage", &body) => {
                    match result {
                        Ok(value) => self.verify_send(session, &record.conversation, record.thread.as_deref(), &value).await,
                        Err(Error::Upstream | Error::Unavailable) => Err(Error::DeliveryUnknown),
                        Err(error) => Err(error)
                    }
                }
            }
        };
        if let Ok((_, Some(message))) = &outcome {
            self.warm_message_media(session, std::slice::from_ref(message)).await;
        }
        let _ = self
            .update_cache(session, move |cache, account| {
                let record = cache.sends.get_mut(&id).ok_or(Error::SendRecord)?;
                match outcome {
                    Ok((ts, message)) => {
                        record.state = SendState::Sent;
                        record.ts = Some(ts);
                        if let Some(mut message) = message
                            && let Some(conversation) =
                                cache.conversations.get_mut(&record.conversation)
                        {
                            message.author.get_or_insert_with(|| Author {
                                id: account.identity.user_id.clone(),
                                name: account.identity.user_id.clone(),
                            });
                            conversation.ingest(message);
                        }
                    }
                    Err(error) => {
                        record.state = if matches!(error, Error::DeliveryUnknown | Error::DeliveryTargetMismatch) {
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

    async fn verify_send(
        &self,
        session: Uuid,
        conversation: &str,
        thread: Option<&str>,
        response: &Value,
    ) -> Result<(String, Option<Message>), Error> {
        let ts = response["ts"]
            .as_str()
            .filter(|ts| timestamp(ts).is_some())
            .ok_or(Error::DeliveryUnknown)?;
        let message = Message::from_slack(&response["message"], None)
            .filter(|message| message.ts == ts);
        let Some(thread) = thread else {
            return Ok((ts.to_owned(), message));
        };
        if response["channel"].as_str().is_some_and(|channel| channel != conversation) {
            return Err(Error::DeliveryTargetMismatch);
        }
        if let Some(root) = response["message"]["thread_ts"].as_str() {
            if root != thread || ts == root {
                return Err(Error::DeliveryTargetMismatch);
            }
            if message.is_some() {
                return Ok((ts.to_owned(), message));
            }
        }
        let message = self
            .find_message(session, conversation, Some(thread), ts)
            .await
            .ok()
            .flatten()
            .ok_or(Error::DeliveryUnknown)?;
        Ok((ts.to_owned(), Some(message)))
    }
}
