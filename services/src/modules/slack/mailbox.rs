use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    protocol::{Error, Message, Page, timestamp, valid_id},
    service::{SlackService, now},
    store::{Account, Cache, Conversation, SendRecord, SendState},
};

#[derive(Deserialize, Serialize)]
struct Cursor {
    session: Uuid,
    target: String,
    anchor: String,
    expires: u64,
}

pub(super) fn read_cursor(
    page: &Page,
    session: Uuid,
    target: &str,
) -> Result<Option<String>, Error> {
    let Some(cursor) = &page.cursor else {
        return Ok(None);
    };
    if cursor.len() > 1_024 {
        return Err(Error::Cursor);
    }
    let decoded = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| Error::Cursor)?;
    let cursor: Cursor = serde_json::from_slice(&decoded).map_err(|_| Error::Cursor)?;
    if cursor.session != session || cursor.target != target || cursor.expires <= now() {
        return Err(Error::Cursor);
    }
    Ok(Some(cursor.anchor))
}

pub(super) fn next_cursor(session: Uuid, target: String, anchor: String) -> String {
    let cursor = Cursor {
        session,
        target,
        anchor,
        expires: now() + 3_600,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor).expect("Slack 游标可序列化"))
}

#[derive(Clone, Serialize)]
pub(super) struct UserProfile {
    pub id: String,
    team_id: String,
    pub name: String,
    is_bot: bool,
    pub display_name: Option<String>,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
}

impl UserProfile {
    fn from_slack(team_id: &str, id: &str, user: &Value) -> Self {
        let profile = &user["profile"];
        let name = [&profile["display_name"], &user["real_name"], &user["name"]]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|name| !name.trim().is_empty())
            .unwrap_or(id);
        Self {
            id: format!("{team_id}.{id}"),
            team_id: team_id.to_owned(),
            name: name.to_owned(),
            is_bot: user["is_bot"] == true,
            display_name: profile["display_name"].as_str().map(str::to_owned),
            username: user["name"].as_str().map(str::to_owned),
            avatar_url: [&profile["image_48"], &profile["image_72"]]
                .into_iter()
                .filter_map(Value::as_str)
                .find(|url| !url.is_empty())
                .map(str::to_owned),
        }
    }

    pub fn slack_id(&self) -> &str {
        &self.id[self.team_id.len() + 1..]
    }
}

pub(super) struct UserDirectory {
    pub session: Uuid,
    expires_at: Option<u64>,
    pub users: std::sync::Arc<[UserProfile]>,
}

impl UserDirectory {
    pub fn complete(&self) -> bool {
        self.expires_at.is_some()
    }

    fn find(&self, id: &str) -> Option<&UserProfile> {
        self.users
            .binary_search_by(|user| user.slack_id().cmp(id))
            .ok()
            .map(|index| &self.users[index])
    }

    fn update(&mut self, profile: UserProfile, deleted: bool) {
        let mut users = self.users.to_vec();
        match (
            users.binary_search_by(|user| user.id.cmp(&profile.id)),
            deleted,
        ) {
            (Ok(index), false) => users[index] = profile,
            (Ok(index), true) => {
                users.remove(index);
            }
            (Err(index), false) => users.insert(index, profile),
            (Err(_), true) => return,
        }
        self.users = users.into();
    }
}

pub(super) struct ConversationRow {
    pub order: Option<(u64, u32)>,
    pub id: String,
    pub value: Value,
}

impl SlackService {
    pub(super) async fn users(
        &self,
        session: Uuid,
    ) -> Result<std::sync::Arc<[UserProfile]>, Error> {
        let _fetch = self.user_fetch.lock().await;
        let team_id = {
            let state = self.state.lock().await;
            let team_id = &state.account(session)?.identity.team_id;
            if let Some(current) = state.user_directory.as_ref()
                && current.session == session
                && current.expires_at.is_some_and(|expires| expires > now())
            {
                return Ok(current.users.clone());
            }
            team_id.clone()
        };
        let mut cursor = String::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut users = BTreeMap::new();
        loop {
            let response = self
                .slack_get(
                    session,
                    "users.list",
                    &[("limit", "200"), ("cursor", &cursor)],
                )
                .await?;
            let members = response["members"].as_array().ok_or(Error::Upstream)?;
            for user in members {
                let Some(id) = user["id"].as_str().filter(|id| valid_id(id)) else {
                    continue;
                };
                if user["deleted"] == true {
                    continue;
                }
                let profile = UserProfile::from_slack(&team_id, id, user);
                users.insert(profile.id.clone(), profile);
            }
            let next = response["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            if next.is_empty() {
                break;
            }
            if !seen.insert(next.to_owned()) {
                return Err(Error::Upstream);
            }
            cursor = next.to_owned();
        }
        let mut state = self.state.lock().await;
        state.account(session)?;
        let users: std::sync::Arc<[UserProfile]> = users.into_values().collect();
        state.user_directory = Some(UserDirectory {
            session,
            expires_at: Some(now() + 300),
            users: users.clone(),
        });
        self.picker_changed.notify_one();
        Ok(users)
    }

    pub(super) async fn lookup_user(&self, session: Uuid, id: &str) -> Result<UserProfile, Error> {
        if let Some(profile) = self.cached_user(session, id).await? {
            return Ok(profile);
        }
        let _fetch = self.user_fetch.lock().await;
        if let Some(profile) = self.cached_user(session, id).await? {
            return Ok(profile);
        }
        let response = self
            .slack_get(session, "users.info", &[("user", id)])
            .await?;
        let user = &response["user"];
        if user["id"].as_str() != Some(id) {
            return Err(Error::Upstream);
        }
        let mut state = self.state.lock().await;
        let account = state.account(session)?;
        let profile = UserProfile::from_slack(&account.identity.team_id, id, user);
        if user["deleted"] != true {
            let directory = state.user_directory.get_or_insert_with(|| UserDirectory {
                session,
                expires_at: None,
                users: Default::default(),
            });
            directory.update(profile.clone(), false);
            self.picker_changed.notify_one();
        }
        Ok(profile)
    }

    async fn cached_user(&self, session: Uuid, id: &str) -> Result<Option<UserProfile>, Error> {
        let state = self.state.lock().await;
        state.account(session)?;
        Ok(state
            .user_directory
            .as_ref()
            .filter(|directory| directory.session == session)
            .and_then(|directory| directory.find(id))
            .cloned())
    }

    pub(super) async fn update_directory_user(&self, session: Uuid, id: &str, user: &Value) {
        if !user.is_object() || !valid_id(id) {
            return;
        }
        let _fetch = self.user_fetch.lock().await;
        let mut state = self.state.lock().await;
        let Ok(account) = state.account(session) else {
            return;
        };
        let profile = UserProfile::from_slack(&account.identity.team_id, id, user);
        let Some(directory) = state
            .user_directory
            .as_mut()
            .filter(|current| current.session == session)
        else {
            return;
        };
        directory.update(profile, user["deleted"] == true);
        self.picker_changed.notify_one();
    }

    async fn directory(
        &self,
        session: Uuid,
        types: &str,
    ) -> Result<BTreeMap<String, Conversation>, Error> {
        let mut directory = BTreeMap::new();
        let mut cursor = String::new();
        loop {
            let response = self
                .slack_get(
                    session,
                    "users.conversations",
                    &[
                        ("types", types),
                        ("exclude_archived", "true"),
                        ("limit", "100"),
                        ("cursor", &cursor),
                    ],
                )
                .await?;
            let channels = response["channels"].as_array().ok_or(Error::Upstream)?;
            for channel in channels {
                let Some(id) = channel["id"].as_str().filter(|id| valid_id(id)) else {
                    continue;
                };
                let kind = if channel["is_im"] == true {
                    "im"
                } else if channel["is_mpim"] == true {
                    "mpim"
                } else if channel["is_private"] == true || channel["is_group"] == true {
                    "private_channel"
                } else if channel["is_channel"] == true {
                    "public_channel"
                } else {
                    continue;
                };
                let mut participants: Vec<_> = channel["members"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                if let Some(user) = channel["user"].as_str() {
                    participants.push(user.to_owned());
                }
                let title = channel["name"]
                    .as_str()
                    .or(channel["user"].as_str())
                    .unwrap_or(id);
                let mut conversation = Conversation::new(id, kind, title);
                conversation.participants = participants;
                directory.insert(id.to_owned(), conversation);
            }
            let next = response["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            if next.is_empty() {
                break;
            }
            if next == cursor {
                return Err(Error::Upstream);
            }
            cursor = next.into();
        }
        Ok(directory)
    }

    pub(super) async fn refresh_directory(&self, session: Uuid) -> Result<(), Error> {
        let directory = match self
            .directory(session, "im,mpim,public_channel,private_channel")
            .await
        {
            // 已授权的旧凭据可能只有私信权限。
            Err(Error::Permission) => self.directory(session, "im,mpim").await?,
            result => result?,
        };
        self.update_cache(session, move |cache, _| {
            cache
                .conversations
                .retain(|id, _| directory.contains_key(id));
            for (id, entry) in directory {
                cache
                    .conversations
                    .entry(id)
                    .and_modify(|current| {
                        current.kind.clone_from(&entry.kind);
                        if (!entry.participants.is_empty()
                            && current.participants != entry.participants)
                            || matches!(entry.kind.as_str(), "public_channel" | "private_channel")
                        {
                            current.participants.clone_from(&entry.participants);
                            current.title.clone_from(&entry.title);
                        }
                    })
                    .or_insert(entry);
            }
            cache.directory_loaded = true;
            Ok(())
        })
        .await
    }

    pub(super) async fn refresh_group_members(&self, session: Uuid) -> Result<(), Error> {
        let groups: Vec<_> = {
            let state = self.state.lock().await;
            state.account(session)?;
            state
                .cache
                .conversations
                .values()
                .filter(|room| room.kind == "mpim" && room.participants.is_empty())
                .map(|room| room.id.clone())
                .collect()
        };
        let mut members = BTreeMap::new();
        for id in groups {
            match self.conversation_members(session, &id).await {
                Ok(users) if !users.is_empty() => {
                    members.insert(id, users);
                }
                Err(
                    Error::RateLimited(_)
                    | Error::Session
                    | Error::Authorization
                    | Error::Unavailable,
                ) => break,
                _ => {}
            }
        }
        if members.is_empty() {
            return Ok(());
        }
        self.update_cache(session, move |cache, _| {
            for (id, users) in members {
                if let Some(room) = cache.conversations.get_mut(&id)
                    && room.participants.is_empty()
                {
                    room.participants = users;
                }
            }
            Ok(())
        })
        .await
    }

    async fn conversation_members(&self, session: Uuid, id: &str) -> Result<Vec<String>, Error> {
        let mut members = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut cursor = String::new();
        loop {
            let page = self
                .slack_get(
                    session,
                    "conversations.members",
                    &[("channel", id), ("limit", "100"), ("cursor", &cursor)],
                )
                .await?;
            members.extend(
                page["members"]
                    .as_array()
                    .ok_or(Error::Upstream)?
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|id| valid_id(id))
                    .map(str::to_owned),
            );
            let next = page["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            if next.is_empty() {
                return Ok(members.into_iter().collect());
            }
            if !seen.insert(next.to_owned()) {
                return Err(Error::Upstream);
            }
            cursor = next.to_owned();
        }
    }

    pub(super) async fn conversation_rows(
        &self,
        session: Uuid,
        unread: bool,
        kind: &str,
    ) -> Result<(Vec<ConversationRow>, bool), Error> {
        if !self.state.lock().await.cache.directory_loaded {
            self.refresh_directory(session).await?;
        }
        let state = self.state.lock().await;
        let account = state.account(session)?;
        let rows = state
            .cache
            .conversations
            .values()
            .filter(|value| kind.is_empty() || value.kind == kind)
            .filter_map(|value| {
                let (latest, unread_time, summary) =
                    self.conversation_summary(&state.cache, account, value);
                if unread && unread_time.is_none() {
                    return None;
                }
                Some(ConversationRow {
                    order: if unread { unread_time } else { latest },
                    id: value.id.clone(),
                    value: summary,
                })
            })
            .collect();
        Ok((rows, state.socket_state == "connected"))
    }

    pub(super) fn conversation_summary(
        &self,
        cache: &Cache,
        account: &Account,
        room: &Conversation,
    ) -> (Option<(u64, u32)>, Option<(u64, u32)>, Value) {
        let (latest, unread, mut summary) = room.summarize(account, cache.positions(&room.id));
        summary["id"] = json!(format!("{}.{}", account.identity.team_id, room.id));
        summary["team_id"] = json!(account.identity.team_id);
        summary["team_name"] = json!(account.identity.team_name);
        summary["session_id"] = json!(account.identity.session_id);
        (latest, unread, summary)
    }

    pub(super) async fn refresh_latest(&self, session: Uuid, id: &str) -> Result<(), Error> {
        self.fetch_messages(session, id, None, None, 50).await?;
        Ok(())
    }

    pub(super) async fn open_target(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<&str>,
        message: Option<&str>,
    ) -> Result<Value, Error> {
        let exists = {
            let state = self.state.lock().await;
            state.account(session)?;
            state.cache.conversations.contains_key(id)
        };
        if !exists {
            self.refresh_directory(session).await?;
        }
        let room = {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            let room = state
                .cache
                .conversations
                .get(id)
                .ok_or(Error::Conversation)?;
            self.conversation_summary(&state.cache, account, room).2
        };
        let root = match thread.or(message) {
            Some(ts) => self.find_message(session, id, None, ts).await?,
            None => None,
        };
        let reply = match (thread, message, root.as_ref()) {
            (Some(thread), Some(ts), Some(_)) if ts != thread => {
                self.find_message(session, id, Some(thread), ts).await?
            }
            _ => None,
        };
        self.update_cache(session, |cache, _| {
            let room = cache.conversations.get_mut(id).ok_or(Error::Conversation)?;
            for message in root.iter().chain(reply.iter()) {
                room.ingest(message.clone());
            }
            Ok(())
        })
        .await?;
        let anchored_thread = thread.filter(|_| root.is_some());
        let anchored_message = match (thread, message, root.as_ref(), reply.as_ref()) {
            (None, Some(ts), Some(_), _) => Some(ts),
            (Some(thread), Some(ts), Some(_), _) if thread == ts => Some(ts),
            (_, Some(ts), _, Some(_)) => Some(ts),
            _ => None,
        };
        Ok(json!({"room":room,"root":root,"reply":reply,
            "thread_ts":anchored_thread,"message_ts":anchored_message}))
    }

    pub(super) async fn attachment_preview(
        &self,
        session: Uuid,
        id: &str,
        ts: &str,
        index: usize,
    ) -> Result<(String, Vec<u8>), Error> {
        let (url, token) = {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            let url = state
                .cache
                .conversations
                .get(id)
                .and_then(|room| room.messages.get(ts))
                .and_then(|message| message.attachments.get(index))
                .filter(|attachment| attachment.image)
                .and_then(|attachment| attachment.image_url.clone())
                .ok_or(Error::Attachment)?;
            (url, account.access.clone())
        };
        let web = self.web_session(session).await?;
        let result = self.client.image(&url, &token, web.as_ref()).await?;
        self.state.lock().await.account(session)?;
        Ok(result)
    }

    async fn find_message(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<&str>,
        ts: &str,
    ) -> Result<Option<Message>, Error> {
        let (method, params) = match thread {
            Some(root) => (
                "conversations.replies",
                vec![
                    ("channel", id),
                    ("ts", root),
                    ("oldest", ts),
                    ("latest", ts),
                    ("inclusive", "true"),
                    ("limit", "1"),
                ],
            ),
            None => (
                "conversations.history",
                vec![
                    ("channel", id),
                    ("latest", ts),
                    ("inclusive", "true"),
                    ("limit", "1"),
                ],
            ),
        };
        let state = self.state.lock().await;
        state.account(session)?;
        let room = state
            .cache
            .conversations
            .get(id)
            .ok_or(Error::Conversation)?;
        if room.deleted.contains(ts) {
            return Ok(None);
        }
        let cached = room
            .messages
            .get(ts)
            .filter(|message| message.thread_ts.as_deref() == thread)
            .cloned();
        drop(state);
        let response = match self.slack_get(session, method, &params).await {
            Ok(response) => response,
            Err(Error::Thread) => return Ok(None),
            Err(
                Error::Unavailable | Error::RateLimited(_) | Error::Upstream | Error::Permission,
            ) => return Ok(cached),
            Err(error) => return Err(error),
        };
        Ok(response["messages"]
            .as_array()
            .ok_or(Error::Upstream)?
            .iter()
            .filter_map(|item| Message::from_slack(item, thread))
            .find(|message| message.ts == ts && message.thread_ts.as_deref() == thread))
    }

    pub(super) async fn messages(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        page: Page,
    ) -> Result<Value, Error> {
        if page.kind.is_some()
            || page.unread.is_some()
            || !valid_id(&id)
            || thread.as_ref().is_some_and(|ts| timestamp(ts).is_none())
        {
            return Err(Error::Invalid);
        }
        let limit = page.limit()?;
        let target = format!("messages:{id}:{}", thread.as_deref().unwrap_or_default());
        let anchor = read_cursor(&page, session, &target)?;
        if anchor.as_ref().is_some_and(|ts| timestamp(ts).is_none()) {
            return Err(Error::Cursor);
        }
        let (items, root, more, covered, live) = match self
            .fetch_messages(session, &id, thread.as_deref(), anchor.as_deref(), limit)
            .await
        {
            Ok((items, root, more)) => {
                let first = items.first().map(|message| message.ts.clone());
                let last = items.last().map(|message| message.ts.clone());
                let covered = first.map(|first| json!({"from_ts":first,"through_ts":last}));
                (items, root, more, covered, true)
            }
            Err(error @ (Error::Unavailable | Error::Upstream | Error::RateLimited(_))) => {
                let state = self.state.lock().await;
                state.account(session)?;
                let conversation = state.cache.conversations.get(&id).ok_or(error)?;
                let root = thread
                    .as_ref()
                    .map(|root| conversation.messages.get(root).cloned().ok_or(error))
                    .transpose()?;
                let mut items: Vec<_> = conversation
                    .messages
                    .values()
                    .filter(|message| message.thread_ts == thread)
                    .filter(|message| {
                        anchor
                            .as_ref()
                            .is_none_or(|anchor| timestamp(&message.ts) < timestamp(anchor))
                    })
                    .cloned()
                    .collect();
                if items.is_empty()
                    && (anchor.is_some()
                        || !conversation
                            .loaded
                            .contains_key(thread.as_deref().unwrap_or_default()))
                {
                    return Err(error);
                }
                items.sort_by_key(|item| timestamp(&item.ts));
                let start = items.len().saturating_sub(limit);
                let more = start > 0
                    || conversation
                        .loaded
                        .get(thread.as_deref().unwrap_or_default())
                        .copied()
                        .unwrap_or(true);
                items.drain(..start);
                (items, root, more, None, false)
            }
            Err(error) => return Err(error),
        };
        let next = if more {
            items
                .first()
                .map(|message| next_cursor(session, target, message.ts.clone()))
        } else {
            None
        };
        let state = self.state.lock().await;
        let account = state.account(session)?;
        let conversation = state
            .cache
            .conversations
            .get(&id)
            .ok_or(Error::Conversation)?;
        let positions = state.cache.positions(&id);
        let mut result = json!({
            "api_version":"1","conversation_id":id,"items":items,"next_cursor":next,
            "sync_state":if live { "live" } else { "cached" },
            "read_through_ts":positions.get(thread.as_deref()),"covered_range":covered,
            "first_unread_ts":conversation.messages.values()
                .filter(|message| message.thread_ts == thread && positions.unread(message, account))
                .min_by_key(|message| timestamp(&message.ts)).map(|message| &message.ts)
        });
        if thread.is_some() {
            result["root"] = json!(root);
        }
        Ok(result)
    }

    async fn fetch_messages(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Message>, Option<Message>, bool), Error> {
        let exists = {
            let state = self.state.lock().await;
            state.account(session)?;
            state.cache.conversations.contains_key(id)
        };
        if !exists {
            self.refresh_directory(session).await?;
            if !self.state.lock().await.cache.conversations.contains_key(id) {
                return Err(Error::Conversation);
            }
        }
        if let Some(thread) = thread {
            return self.fetch_thread(session, id, thread, before, limit).await;
        }
        let number = limit.to_string();
        let mut latest = before.map(str::to_owned);
        let mut cursor = String::new();
        let mut seen_cursors = std::collections::BTreeSet::new();
        let (items, more) = loop {
            let mut params = vec![("channel", id), ("limit", number.as_str())];
            if let Some(latest) = &latest {
                params.push(("latest", latest));
            }
            if !cursor.is_empty() {
                params.push(("cursor", &cursor));
            }
            let response = self
                .slack_get(session, "conversations.history", &params)
                .await?;
            let next = response["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            let more = !next.is_empty() || response["has_more"] == true;
            let messages = response["messages"].as_array().ok_or(Error::Upstream)?;
            let mut items: Vec<_> = messages
                .iter()
                .filter_map(|value| Message::from_slack(value, None))
                .filter(|message| {
                    message.thread_ts.is_none()
                        && latest
                            .as_deref()
                            .is_none_or(|latest| timestamp(&message.ts) < timestamp(latest))
                })
                .collect();
            if !items.is_empty() || !more {
                items.sort_by_key(|message| timestamp(&message.ts));
                break (items, more);
            }
            // 过滤掉线程广播后，仍需继续寻找顶层消息。
            if !next.is_empty() {
                if !seen_cursors.insert(next.to_owned()) {
                    return Err(Error::Upstream);
                }
                cursor = next.to_owned();
            } else {
                let boundary = messages
                    .iter()
                    .filter_map(|message| message["ts"].as_str())
                    .filter_map(|ts| timestamp(ts).map(|time| (time, ts)))
                    .filter(|(time, _)| {
                        latest
                            .as_deref()
                            .and_then(timestamp)
                            .is_none_or(|latest| *time < latest)
                    })
                    .min_by_key(|(time, _)| *time)
                    .ok_or(Error::Upstream)?;
                latest = Some(boundary.1.to_owned());
                cursor.clear();
                seen_cursors.clear();
            }
        };
        self.store_page(session, id, None, &items, None, more)
            .await?;
        Ok((items, None, more))
    }

    async fn fetch_thread(
        &self,
        session: Uuid,
        id: &str,
        thread: &str,
        before: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<Message>, Option<Message>, bool), Error> {
        // Slack 按从早到晚返回回复。保留最后一页所需的窗口，并始终取得根消息。
        let mut window = std::collections::VecDeque::new();
        let mut root = None;
        let mut count = 0usize;
        let mut cursor = String::new();
        let mut oldest: Option<String> = None;
        let mut last_reply = None;
        let mut seen_cursors = std::collections::BTreeSet::new();
        loop {
            let mut params = vec![("channel", id), ("ts", thread), ("limit", "100")];
            if let Some(before) = before {
                params.push(("latest", before));
            }
            if !cursor.is_empty() {
                params.push(("cursor", &cursor));
            }
            if let Some(oldest) = &oldest {
                params.push(("oldest", oldest));
            }
            let response = self
                .slack_get(session, "conversations.replies", &params)
                .await?;
            for message in response["messages"]
                .as_array()
                .ok_or(Error::Upstream)?
                .iter()
                .filter_map(|value| Message::from_slack(value, Some(thread)))
            {
                if message.ts == thread && message.thread_ts.is_none() {
                    root = Some(message);
                } else if message.thread_ts.as_deref() == Some(thread)
                    && before.is_none_or(|before| timestamp(&message.ts) < timestamp(before))
                    && last_reply.is_none_or(|last| timestamp(&message.ts) > Some(last))
                {
                    last_reply = timestamp(&message.ts);
                    count += 1;
                    if window.len() == limit {
                        window.pop_front();
                    }
                    window.push_back(message);
                }
            }
            let next = response["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default();
            if !next.is_empty() {
                if !seen_cursors.insert(next.to_owned()) {
                    return Err(Error::Upstream);
                }
                cursor = next.to_owned();
            } else if response["has_more"] == true {
                let latest_reply = window.back().ok_or(Error::Upstream)?;
                if oldest.as_deref().and_then(timestamp) >= timestamp(&latest_reply.ts) {
                    return Err(Error::Upstream);
                }
                oldest = Some(latest_reply.ts.clone());
                cursor.clear();
                seen_cursors.clear();
            } else {
                break;
            }
        }
        let root = root.ok_or(Error::Thread)?;
        let items: Vec<_> = window.into_iter().collect();
        let more = count > limit;
        self.store_page(session, id, Some(thread), &items, Some(&root), more)
            .await?;
        Ok((items, Some(root), more))
    }

    async fn store_page(
        &self,
        session: Uuid,
        id: &str,
        view: Option<&str>,
        items: &[Message],
        root: Option<&Message>,
        more: bool,
    ) -> Result<(), Error> {
        let id = id.to_owned();
        let view = view.map(str::to_owned);
        let items = items.to_vec();
        let root = root.cloned();
        self.update_cache(session, move |cache, _| {
            let conversation = cache
                .conversations
                .get_mut(&id)
                .ok_or(Error::Conversation)?;
            let marker = view.clone().unwrap_or_default();
            conversation.loaded.insert(marker, more);
            if let (Some(first), Some(last)) = (items.first(), items.last()) {
                let min = timestamp(&first.ts);
                let max = timestamp(&last.ts);
                let vanished: Vec<_> = conversation
                    .messages
                    .values()
                    .filter(|message| {
                        message.thread_ts == view
                            && timestamp(&message.ts) >= min
                            && timestamp(&message.ts) <= max
                            && !items.iter().any(|current| current.ts == message.ts)
                    })
                    .map(|message| message.ts.clone())
                    .collect();
                for ts in vanished {
                    conversation.remove(&ts);
                }
            }
            for message in items.into_iter().chain(root) {
                conversation.ingest(message);
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn mark_read(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        target: String,
    ) -> Result<Value, Error> {
        let position = timestamp(&target).ok_or(Error::Invalid)?;
        if !valid_id(&id) || thread.as_ref().is_some_and(|ts| timestamp(ts).is_none()) {
            return Err(Error::Invalid);
        }
        self.update_cache(session, move |cache, account| {
            let conversation = cache.conversations.get(&id).ok_or(Error::Conversation)?;
            let existing = cache.positions(&id).get(thread.as_deref()).and_then(timestamp);
            if existing.is_none_or(|value| position > value) {
                if !conversation.messages.get(&target).is_some_and(|message| message.thread_ts == thread) {
                    return Err(Error::ReadPosition);
                }
                cache.read_positions.entry(id.clone()).or_default().0.insert(thread.clone().unwrap_or_default(), target);
            }
            let positions = cache.positions(&id);
            let (_, _, summary) = conversation.summarize(account, positions);
            Ok(json!({"api_version":"1","conversation_id":id,"thread_ts":thread,
                "read_through_ts":positions.get(thread.as_deref()),"unread_count":summary["unread_count"]}))
        }).await
    }

    pub(super) async fn enqueue(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        key: Uuid,
        text: String,
    ) -> Result<(bool, Value), Error> {
        if !valid_id(&id)
            || thread.as_ref().is_some_and(|ts| timestamp(ts).is_none())
            || text.trim().is_empty()
            || text.chars().count() > 4_000
        {
            return Err(Error::Invalid);
        }
        let fingerprint = URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()));
        let mut verified_root = false;
        if let Some(thread) = &thread {
            let needs_lookup = {
                let state = self.state.lock().await;
                state.account(session)?;
                let cache = &state.cache;
                if cache.sends.contains_key(&key) {
                    false
                } else {
                    let conversation = cache.conversations.get(&id).ok_or(Error::Conversation)?;
                    if conversation.deleted.contains(thread) {
                        return Err(Error::Thread);
                    }
                    !conversation.messages.contains_key(thread)
                }
            };
            if needs_lookup {
                let response = self
                    .slack_get(
                        session,
                        "conversations.replies",
                        &[("channel", &id), ("ts", thread), ("limit", "1")],
                    )
                    .await?;
                verified_root = response["messages"]
                    .as_array()
                    .ok_or(Error::Upstream)?
                    .iter()
                    .filter_map(|value| Message::from_slack(value, Some(thread)))
                    .any(|message| message.ts == *thread && message.thread_ts.is_none());
                if !verified_root {
                    return Err(Error::Thread);
                }
            }
        }
        let result = self
            .update_cache(session, move |cache, _| {
                if let Some(record) = cache.sends.get(&key) {
                    if record.conversation != id
                        || record.thread != thread
                        || record.fingerprint != fingerprint
                    {
                        return Err(Error::Conflict);
                    }
                    return Ok((record.pending(), record.json()));
                }
                let conversation = cache.conversations.get(&id).ok_or(Error::Conversation)?;
                if let Some(thread) = &thread
                    && (conversation.deleted.contains(thread)
                        || !conversation
                            .messages
                            .get(thread)
                            .map_or(verified_root, |root| root.thread_ts.is_none()))
                {
                    return Err(Error::Thread);
                }
                let record = SendRecord {
                    id: key,
                    conversation: id,
                    thread,
                    fingerprint,
                    text: Some(text),
                    state: SendState::Pending,
                    ts: None,
                    failure: None,
                };
                let response = record.json();
                cache.sends.insert(key, record);
                Ok((true, response))
            })
            .await?;
        if result.0 {
            self.outgoing.notify_one();
        }
        Ok(result)
    }

    pub(super) async fn send_record(&self, session: Uuid, id: Uuid) -> Result<Value, Error> {
        let state = self.state.lock().await;
        state.account(session)?;
        state
            .cache
            .sends
            .get(&id)
            .map(SendRecord::json)
            .ok_or(Error::SendRecord)
    }
}
