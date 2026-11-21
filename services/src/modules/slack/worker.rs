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
        let _ = self.initialize().await;
        self.restore_app_token().await;
        self.restore_user_token().await;
        self.publish_status().await;
        tokio::join!(
            self.sockets(),
            self.sending(),
            self.synchronizing(),
            self.notifications()
        );
    }

    async fn restore_user_token(&self) {
        if let Err(error) = self.restore_imported_token().await {
            self.state.lock().await.issue = Some(error);
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
                            .first()
                            .cloned()
                            .map(|id| (session, id))
                    })
            };
            let Some((session, id)) = pending else {
                self.alerts.notified().await;
                continue;
            };
            let delivered = self
                .notification
                .send(NotificationRequest {
                    title: "Slack 新消息".into(),
                    body: None,
                    app: None,
                    callback_url: None,
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
            let (token, session, locked) = {
                let state = self.state.lock().await;
                (
                    state.app_token.clone(),
                    state
                        .secrets
                        .account
                        .as_ref()
                        .filter(|account| !account.revoked && !account.access.is_empty())
                        .map(|account| account.identity.session_id),
                    state.credentials_locked,
                )
            };
            let has_token = token.is_some();
            let has_session = session.is_some();
            let (Some(token), Some(session), false) = (token, session, locked) else {
                self.state.lock().await.socket_state = "stopped";
                self.publish_status().await;
                tokio::select! {
                    _ = changes.changed() => { backoff = 1; }
                    _ = tokio::time::sleep(Duration::from_secs(15)), if locked || !has_token || !has_session => {
                        if locked && self.initialize().await.is_ok() { self.signal_change(); }
                        if !has_token { self.restore_app_token().await; }
                        if !has_session || locked { self.restore_user_token().await; }
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
                let socket = self.client.connect(&token).await?;
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
            let incoming = tokio::time::timeout(Duration::from_secs(90), socket.next())
                .await
                .map_err(|_| Error::Unavailable)?
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
                Some("disconnect") => return Ok(()),
                _ => {}
            }
            let notify = if event["type"] == "events_api" {
                self.accept_event(session, &event["payload"]).await?
            } else {
                false
            };
            if notify {
                self.alerts.notify_one();
            }
            if let Some(envelope) = event["envelope_id"].as_str() {
                socket
                    .send(Frame::Text(
                        json!({"envelope_id":envelope}).to_string().into(),
                    ))
                    .await
                    .map_err(|_| Error::Unavailable)?;
            }
        }
    }

    pub(super) async fn accept_event(&self, session: Uuid, payload: &Value) -> Result<bool, Error> {
        let event = &payload["event"];
        let event_type = event["type"].as_str().unwrap_or_default();
        if matches!(event_type, "app_uninstalled" | "tokens_revoked") {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            let revoked_user = event["tokens"]["oauth"].as_array().is_some_and(|users| {
                users
                    .iter()
                    .any(|user| user.as_str() == Some(&account.identity.user_id))
            });
            let revoked = payload["team_id"].as_str() == Some(&account.identity.team_id)
                && (event_type == "app_uninstalled" || revoked_user);
            let access = account.access.clone();
            drop(state);
            if revoked {
                self.revoke(session, &access).await;
            }
            return Ok(false);
        }
        if event_type != "message" {
            return Ok(false);
        }
        let Some(id) = event["channel"].as_str().filter(|id| valid_id(id)) else {
            return Ok(false);
        };
        let kind = match event["channel_type"].as_str() {
            Some("im") => "im",
            Some("mpim") => "mpim",
            _ => return Ok(false),
        };
        let Some(event_id) = payload["event_id"].as_str().filter(|id| valid_id(id)) else {
            return Ok(false);
        };
        let id = id.to_owned();
        let event_id = event_id.to_owned();
        let team = payload["team_id"].as_str().unwrap_or_default().to_owned();
        let authorized_users: Vec<_> = payload["authorizations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|authorization| authorization["is_bot"] == false)
            .filter_map(|authorization| authorization["user_id"].as_str().map(str::to_owned))
            .collect();
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
        self.update_cache(session, move |cache, account| {
            if team != account.identity.team_id
                || !authorized_users.contains(&account.identity.user_id)
                || !cache.events.insert(event_id.clone())
            {
                return Ok(false);
            }
            if cache.events.len() > 10_000 {
                cache.events.pop_first();
            }
            let conversation = cache
                .conversations
                .entry(id.clone())
                .or_insert_with(|| Conversation::new(&id, kind, &id));
            if subtype == "message_deleted" {
                if let Some(deleted) = deleted {
                    conversation.remove(&deleted);
                }
                return Ok(false);
            }
            let Some(message) = message else {
                return Ok(false);
            };
            let notify = new
                && conversation.unread(&message, account)
                && !conversation.deleted.contains(&message.ts);
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
            conversation.ingest(message);
            if notify {
                cache.notifications.insert(event_id);
                if cache.notifications.len() > 512 {
                    cache.notifications.pop_first();
                }
            }
            Ok(notify)
        })
        .await
    }

    async fn synchronizing(&self) {
        loop {
            let reconnect = tokio::select! {
                _ = self.sync.notified() => true,
                _ = tokio::time::sleep(Duration::from_secs(120)) => false,
            };
            let session = {
                let state = self.state.lock().await;
                state
                    .secrets
                    .account
                    .as_ref()
                    .filter(|account| !account.revoked && !account.access.is_empty())
                    .map(|account| account.identity.session_id)
            };
            let Some(session) = session else {
                continue;
            };
            if self.refresh_directory(session).await.is_err() {
                continue;
            }
            self.refresh_names(session).await;
            if !reconnect {
                continue;
            }
            let targets: Vec<_> = {
                let state = self.state.lock().await;
                let Ok(account) = state.account(session) else {
                    continue;
                };
                state
                    .cache
                    .conversations
                    .values()
                    .filter(|item| matches!(item.kind.as_str(), "im" | "mpim"))
                    .map(|item| {
                        let latest = item
                            .messages
                            .values()
                            .filter(|message| message.thread_ts.is_none())
                            .max_by_key(|message| timestamp(&message.ts))
                            .map(|message| message.ts.clone())
                            .unwrap_or_else(|| format!("{}.000000", account.baseline));
                        let threads = item
                            .messages
                            .values()
                            .filter_map(|message| {
                                message.thread_ts.clone().or_else(|| {
                                    (message.reply_count > 0).then(|| message.ts.clone())
                                })
                            })
                            .collect::<std::collections::BTreeSet<_>>();
                        (
                            item.id.clone(),
                            latest,
                            threads.into_iter().rev().take(5).collect::<Vec<_>>(),
                        )
                    })
                    .collect()
            };
            for (id, checkpoint, threads) in targets {
                self.catch_up(session, &id, None, Some(&checkpoint)).await;
                for thread in threads {
                    self.catch_up(session, &id, Some(thread), None).await;
                }
            }
        }
    }

    async fn catch_up(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<String>,
        checkpoint: Option<&str>,
    ) {
        let mut cursor = None;
        loop {
            let Ok(page) = self
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
                .await
            else {
                break;
            };
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
        let mut names = BTreeMap::new();
        for id in ids {
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
                    .filter(|account| !account.revoked && !account.access.is_empty())
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
