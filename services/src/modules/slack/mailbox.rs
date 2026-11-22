use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::{
    disk,
    protocol::{Attachment, Error, Message, Page, timestamp, valid_id},
    service::{SlackService, now},
    store::{Account, Conversation, SendRecord, SendState},
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

#[derive(Clone, Deserialize, Serialize)]
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
    pub fn result(&self) -> Value {
        let mut value = json!(self);
        if let Some(fields) = value.as_object_mut() {
            fields.remove("avatar_url");
        }
        value
    }

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
    pub fn from_disk(session: Uuid, users: Vec<UserProfile>) -> Self {
        Self {
            session,
            expires_at: None,
            users: users.into(),
        }
    }
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
            let response = match self
                .slack_get(
                    session,
                    "users.list",
                    &[("limit", "200"), ("cursor", &cursor)],
                )
                .await
            {
                Ok(response) => response,
                Err(error @ (Error::Unavailable | Error::Upstream | Error::RateLimited(_))) => {
                    let state = self.state.lock().await;
                    state.account(session)?;
                    return state
                        .user_directory
                        .as_ref()
                        .filter(|directory| !directory.users.is_empty())
                        .map(|directory| directory.users.clone())
                        .ok_or(error);
                }
                Err(error) => return Err(error),
            };
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
        let mut state = self.state.clone().lock_owned().await;
        state.account(session)?;
        let users: std::sync::Arc<[UserProfile]> = users.into_values().collect();
        let saved = users.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(disk) = &state.cache.disk {
                let previous = state.user_directory.as_ref();
                for user in saved.iter() {
                    disk.save_user(
                        user,
                        previous.and_then(|directory| directory.find(user.slack_id())),
                    );
                }
                if let Some(previous) = previous {
                    for user in previous.users.iter() {
                        if saved
                            .binary_search_by(|candidate| candidate.id.cmp(&user.id))
                            .is_err()
                        {
                            disk.remove_user(user.slack_id());
                        }
                    }
                }
            }
            state.user_directory = Some(UserDirectory {
                session,
                expires_at: Some(now() + 300),
                users: saved,
            });
        })
        .await
        .map_err(|_| Error::Unavailable)?;
        self.picker_changed.notify_one();
        self.avatars_changed.notify_one();
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
        let mut state = self.state.clone().lock_owned().await;
        let profile = UserProfile::from_slack(&state.account(session)?.identity.team_id, id, user);
        if user["deleted"] != true {
            let saved = profile.clone();
            tokio::task::spawn_blocking(move || {
                if let Some(disk) = &state.cache.disk {
                    disk.save_user(
                        &saved,
                        state
                            .user_directory
                            .as_ref()
                            .and_then(|directory| directory.find(saved.slack_id())),
                    );
                }
                state
                    .user_directory
                    .get_or_insert_with(|| UserDirectory::from_disk(session, Vec::new()))
                    .update(saved, false);
            })
            .await
            .map_err(|_| Error::Unavailable)?;
            self.picker_changed.notify_one();
            self.avatars_changed.notify_one();
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
        let mut state = self.state.clone().lock_owned().await;
        let Ok(account) = state.account(session) else {
            return;
        };
        let profile = UserProfile::from_slack(&account.identity.team_id, id, user);
        let deleted = user["deleted"] == true;
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(disk) = &state.cache.disk {
                if deleted {
                    disk.remove_user(profile.slack_id());
                } else {
                    disk.save_user(
                        &profile,
                        state
                            .user_directory
                            .as_ref()
                            .and_then(|directory| directory.find(profile.slack_id())),
                    );
                }
            }
            state
                .user_directory
                .get_or_insert_with(|| UserDirectory::from_disk(session, Vec::new()))
                .update(profile, deleted);
        })
        .await;
        self.picker_changed.notify_one();
        self.avatars_changed.notify_one();
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
        let _fetch = self.directory_fetch.lock().await;
        let directory = match self
            .directory(session, "im,mpim,public_channel,private_channel")
            .await
        {
            // 已授权的旧凭据可能只有私信权限。
            Err(Error::Permission) => self.directory(session, "im,mpim").await?,
            result => result?,
        };
        self.update_cache(session, move |cache, account| {
            if let Some(disk) = &cache.disk {
                for id in cache
                    .conversations
                    .keys()
                    .filter(|id| !directory.contains_key(*id))
                {
                    disk.remove_room(id);
                }
            }
            cache
                .conversations
                .retain(|id, _| directory.contains_key(id));
            for (id, mut entry) in directory {
                entry.disk.clone_from(&cache.disk);
                entry.read_positions.baseline = Some(account.baseline);
                let room = cache
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
                if let Some(disk) = &cache.disk {
                    let _ = disk.save_room(room);
                }
            }
            cache.directory_loaded = true;
            Ok(())
        })
        .await
    }

    pub(super) async fn open_dm(&self, session: Uuid, user: &str) -> Result<Value, Error> {
        let profile = self.lookup_user(session, user).await?;
        // 等待已开始的目录刷新，避免旧快照移除刚打开的私聊。
        let _fetch = self.directory_fetch.lock().await;
        {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            if let Some(room) = state
                .cache
                .conversations
                .values()
                .find(|room| room.kind == "im" && room.participants.iter().any(|id| id == user))
            {
                let mut summary = self.conversation_summary(account, room).2;
                summary["title"] = json!(profile.name);
                return Ok(summary);
            }
        }
        let response = self
            .slack_post(session, "conversations.open", &json!({"users":user}))
            .await?;
        let id = response["channel"]["id"]
            .as_str()
            .filter(|id| id.starts_with('D') && valid_id(id))
            .ok_or(Error::Upstream)?
            .to_owned();
        let user = user.to_owned();
        let title = profile.name;
        let id = self
            .update_cache(session, move |cache, account| {
                let room = cache.conversations.entry(id.clone()).or_insert_with(|| {
                    let mut room = Conversation::new(&id, "im", &title);
                    room.disk.clone_from(&cache.disk);
                    room.read_positions.baseline = Some(account.baseline);
                    room
                });
                room.kind = "im".into();
                room.title = title;
                room.participants = vec![user];
                if let Some(disk) = &room.disk {
                    let _ = disk.save_room(room);
                }
                Ok(id)
            })
            .await?;
        let state = self.state.lock().await;
        let account = state.account(session)?;
        let room = state
            .cache
            .conversations
            .get(&id)
            .ok_or(Error::Conversation)?;
        Ok(self.conversation_summary(account, room).2)
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
                    if let Some(disk) = &room.disk {
                        let _ = disk.save_room(room);
                    }
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
            let result = self.refresh_directory(session).await;
            if !matches!(
                result,
                Err(Error::Unavailable | Error::Upstream | Error::RateLimited(_))
            ) || self.state.lock().await.cache.conversations.is_empty()
            {
                result?;
            }
        }
        let state = self.state.lock().await;
        let account = state.account(session)?;
        let rows = state
            .cache
            .conversations
            .values()
            .filter(|value| kind.is_empty() || value.kind == kind)
            .filter_map(|value| {
                let (latest, unread_time, summary) = self.conversation_summary(account, value);
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
        account: &Account,
        room: &Conversation,
    ) -> (Option<(u64, u32)>, Option<(u64, u32)>, Value) {
        let (latest, unread, mut summary) = room.summarize(account);
        summary["id"] = json!(format!("{}.{}", account.identity.team_id, room.id));
        summary["team_id"] = json!(account.identity.team_id);
        summary["team_name"] = json!(account.identity.team_name);
        summary["session_id"] = json!(account.identity.session_id);
        if let Some(disk) = &room.disk {
            disk::add_path(&mut summary, "cache_path", disk.room_path(&room.id));
        }
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
            self.conversation_summary(account, room).2
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
        let room_id = id.to_owned();
        let messages: Vec<_> = root.iter().chain(reply.iter()).cloned().collect();
        self.warm_message_media(session, &messages).await;
        self.update_cache(session, move |cache, _| {
            let room = cache
                .conversations
                .get_mut(&room_id)
                .ok_or(Error::Conversation)?;
            for message in messages {
                room.ingest(message);
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
        let state = self.state.lock().await;
        state.account(session)?;
        let conversation = state
            .cache
            .conversations
            .get(id)
            .ok_or(Error::Conversation)?;
        let wire_root = root
            .as_ref()
            .map(|message| conversation.message_result(message));
        let wire_reply = reply
            .as_ref()
            .map(|message| conversation.message_result(message));
        Ok(json!({"room":room,"root":wire_root,"reply":wire_reply,
            "thread_ts":anchored_thread,"message_ts":anchored_message}))
    }

    pub(super) async fn attachment(
        &self,
        session: Uuid,
        id: &str,
        ts: &str,
        index: usize,
        preview: bool,
        local_path: bool,
    ) -> Result<(String, Vec<u8>, Option<std::path::PathBuf>), Error> {
        let (item, token, cache) = {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            let room = state.cache.conversations.get(id).ok_or(Error::Attachment)?;
            if room.deleted.contains(ts) {
                return Err(Error::Attachment);
            }
            let message = room
                .messages
                .get(ts)
                .cloned()
                .or_else(|| room.disk.as_ref()?.find_message(id, ts))
                .ok_or(Error::Attachment)?;
            let item = message
                .attachments
                .into_iter()
                .nth(index)
                .filter(|item| !preview || item.image)
                .ok_or(Error::Attachment)?;
            (item, account.access.clone(), state.cache.disk.clone())
        };
        if let Some(path) = cache
            .as_ref()
            .and_then(|cache| cache.attachment(&item, preview))
        {
            if local_path {
                return Ok((String::new(), Vec::new(), Some(path)));
            }
            if let Some(mime) = path
                .extension()
                .and_then(|ext| ext.to_str())
                .and_then(disk::preview_mime)
                && let Ok(bytes) = tokio::fs::read(&path).await
            {
                return Ok((mime.into(), bytes, Some(path)));
            }
        }
        let url = if preview {
            &item.image_url
        } else {
            &item.download_url
        };
        let url = url.as_deref().ok_or(Error::Attachment)?;
        let web = self.web_session(session).await?;
        if !preview {
            let cache = cache.ok_or(Error::Unavailable)?;
            let (mime, mut response) = self.client.original(url, &token, web.as_ref()).await?;
            let (pending, file) = {
                let state = self.state.lock().await;
                state.account(session)?;
                cache
                    .begin_original(&item, &mime)
                    .map_err(|_| Error::Unavailable)?
            };
            let mut file = tokio::fs::File::from_std(file);
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
                file.write_all(&chunk)
                    .await
                    .map_err(|_| Error::Unavailable)?;
            }
            file.flush().await.map_err(|_| Error::Unavailable)?;
            drop(file);
            let state = self.state.lock().await;
            state.account(session)?;
            let path = pending.commit().map_err(|_| Error::Unavailable)?;
            return Ok((mime, Vec::new(), Some(path)));
        }
        self.download_preview(session, &item, &token, web.as_ref(), cache)
            .await
    }

    pub(super) async fn avatar(
        &self,
        session: Uuid,
        id: &str,
    ) -> Result<std::path::PathBuf, Error> {
        let profile = self.lookup_user(session, id).await?;
        let cache = {
            let state = self.state.lock().await;
            state.account(session)?;
            state.cache.disk.clone().ok_or(Error::Unavailable)?
        };
        let path = cache.avatar_path(id);
        if let Some(path) = cache.cached_avatar(id) {
            return Ok(path);
        }
        let url = profile.avatar_url.as_deref().ok_or(Error::Attachment)?;
        let bytes = self.client.avatar(url).await?;
        let state = self.state.clone().lock_owned().await;
        state.account(session)?;
        if state
            .user_directory
            .as_ref()
            .and_then(|directory| directory.find(id))
            .and_then(|current| current.avatar_url.as_deref())
            != Some(url)
        {
            return Err(Error::Attachment);
        }
        tokio::task::spawn_blocking(move || {
            let png = disk::avatar_png(&bytes).ok_or(Error::Attachment)?;
            disk::atomic_write(&path, &png).map_err(|_| Error::Unavailable)?;
            drop(state);
            Ok(path)
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }

    pub(super) async fn warm_avatars(&self, session: Uuid) {
        let (users, bots) = {
            let state = self.state.lock().await;
            let Some(cache) = state
                .cache
                .disk
                .as_ref()
                .filter(|_| state.account(session).is_ok())
            else {
                return;
            };
            let users = state
                .user_directory
                .iter()
                .flat_map(|directory| directory.users.iter())
                .filter(|user| {
                    user.avatar_url.is_some() && cache.cached_avatar(user.slack_id()).is_none()
                })
                .map(|user| user.slack_id().to_owned())
                .collect::<Vec<_>>();
            let bots = state
                .cache
                .conversations
                .values()
                .flat_map(|room| room.messages.values())
                .filter_map(|message| message.author.as_ref())
                .map(|author| &author.id)
                .filter(|id| id.starts_with('B') && valid_id(id))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .filter(|id| cache.cached_avatar(id).is_none())
                .cloned()
                .collect::<Vec<_>>();
            (users, bots)
        };
        stream::iter(
            users
                .into_iter()
                .map(|id| (id, false))
                .chain(bots.into_iter().map(|id| (id, true))),
        )
        .for_each_concurrent(4, |(id, bot)| async move {
            let result = if bot {
                self.warm_bot_avatar(session, &id).await
            } else {
                self.avatar(session, &id).await.map(|_| ())
            };
            if result.is_ok() {
                self.picker_changed.notify_one();
            }
        })
        .await;
    }

    async fn warm_bot_avatar(&self, session: Uuid, id: &str) -> Result<(), Error> {
        let result = self.slack_get(session, "bots.info", &[("bot", id)]).await?;
        let bot = &result["bot"];
        if bot["id"].as_str() != Some(id) || bot["deleted"] == true {
            return Err(Error::Attachment);
        }
        let url = ["image_48", "image_72", "image_36"]
            .into_iter()
            .filter_map(|key| bot["icons"][key].as_str())
            .find(|url| !url.is_empty())
            .ok_or(Error::Attachment)?;
        let bytes = self.client.avatar(url).await?;
        let state = self.state.clone().lock_owned().await;
        state.account(session)?;
        let path = state
            .cache
            .disk
            .as_ref()
            .ok_or(Error::Unavailable)?
            .avatar_path(id);
        tokio::task::spawn_blocking(move || {
            let png = disk::avatar_png(&bytes).ok_or(Error::Attachment)?;
            let result = disk::atomic_write(&path, &png).map_err(|_| Error::Unavailable);
            drop(state);
            result
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }

    pub(super) async fn warm_message_media(&self, session: Uuid, messages: &[Message]) {
        let (cache, token) = {
            let state = self.state.lock().await;
            let (Ok(account), Some(cache)) = (state.account(session), &state.cache.disk) else {
                return;
            };
            (cache.clone(), account.access.clone())
        };
        let mut seen = std::collections::BTreeSet::new();
        let missing: Vec<_> = messages
            .iter()
            .flat_map(|message| &message.attachments)
            .filter(|item| item.image && cache.attachment(item, true).is_none())
            .filter(|item| {
                item.image_url
                    .as_deref()
                    .is_some_and(|url| seen.insert((item.file_id.as_deref(), url)))
            })
            .collect();
        let icons: std::collections::BTreeSet<_> = messages.iter()
            .filter_map(|message| message.icon_url.as_deref())
            .filter(|url| !disk::regular_file(&cache.message_icon_path(url)))
            .map(str::to_owned)
            .collect();
        let previews = async {
            if missing.is_empty() {
                return;
            }
            let Ok(web) = self.web_session(session).await else {
                return;
            };
            stream::iter(missing).for_each_concurrent(4, |item| async {
                let _ = self
                    .download_preview(session, item, &token, web.as_ref(), Some(cache.clone()))
                    .await;
            }).await;
        };
        let icons = async {
            for url in icons {
                let _ = self.cache_message_icon(session, &cache, &url).await;
            }
        };
        // 图片获取失败或过慢时，消息正文仍可到达聊天界面。
        let _ = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            tokio::join!(icons, previews);
        }).await;
    }

    async fn cache_message_icon(
        &self,
        session: Uuid,
        cache: &disk::DiskCache,
        url: &str,
    ) -> Result<(), Error> {
        let _permit = self.preview_downloads.acquire().await.map_err(|_| Error::Unavailable)?;
        let path = cache.message_icon_path(url);
        if disk::regular_file(&path) {
            return Ok(());
        }
        let bytes = self.client.avatar(url).await?;
        let state = self.state.clone().lock_owned().await;
        state.account(session)?;
        tokio::task::spawn_blocking(move || {
            let png = disk::avatar_png(&bytes).ok_or(Error::Attachment)?;
            let result = disk::atomic_write(&path, &png).map_err(|_| Error::Unavailable);
            drop(state);
            result
        }).await.map_err(|_| Error::Unavailable)?
    }

    async fn download_preview(
        &self,
        session: Uuid,
        item: &Attachment,
        token: &str,
        web: Option<&super::client::WebSession>,
        cache: Option<disk::DiskCache>,
    ) -> Result<(String, Vec<u8>, Option<std::path::PathBuf>), Error> {
        let _permit = self
            .preview_downloads
            .acquire()
            .await
            .map_err(|_| Error::Unavailable)?;
        if let Some(path) = cache
            .as_ref()
            .and_then(|cache| cache.attachment(item, true))
            && let Some(mime) = path
                .extension()
                .and_then(|ext| ext.to_str())
                .and_then(disk::preview_mime)
            && let Ok(bytes) = tokio::fs::read(&path).await
        {
            return Ok((mime.into(), bytes, Some(path)));
        }
        let url = item.image_url.as_deref().ok_or(Error::Attachment)?;
        let (mime, bytes) = self.client.image(url, token, web).await?;
        let state = self.state.clone().lock_owned().await;
        state.account(session)?;
        let Some(cache) = cache else {
            return Ok((mime, bytes, None));
        };
        let item = item.clone();
        tokio::task::spawn_blocking(move || {
            let path = cache.save_preview(&item, &mime, &bytes);
            drop(state);
            (mime, bytes, path)
        })
        .await
        .map_err(|_| Error::Unavailable)
    }

    pub(super) async fn user_result(
        &self,
        session: Uuid,
        user: &UserProfile,
    ) -> Result<Value, Error> {
        let (team_name, disk) = {
            let state = self.state.lock().await;
            let account = state.account(session)?;
            (account.identity.team_name.clone(), state.cache.disk.clone())
        };
        let mut item = disk
            .as_ref()
            .map_or_else(|| user.result(), |cache| cache.user_result(user));
        item["team_name"] = json!(team_name);
        item["session_id"] = json!(session);
        Ok(item)
    }

    pub(super) async fn find_message(
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
                let state = self.state.clone().lock_owned().await;
                state.account(session)?;
                let (id, thread, anchor) = (id.clone(), thread.clone(), anchor.clone());
                tokio::task::spawn_blocking(move || {
                    let conversation = state.cache.conversations.get(&id).ok_or(error)?;
                    let root = thread
                        .as_ref()
                        .map(|root| {
                            conversation
                                .messages
                                .get(root)
                                .cloned()
                                .or_else(|| conversation.disk.as_ref()?.message(&id, None, root))
                                .ok_or(error)
                        })
                        .transpose()?;
                    let mut stored: BTreeMap<_, _> = conversation
                        .disk
                        .as_ref()
                        .into_iter()
                        .flat_map(|disk| disk.messages(&id, thread.as_deref()))
                        .map(|message| (message.ts.clone(), message))
                        .collect();
                    stored.extend(
                        conversation
                            .messages
                            .iter()
                            .map(|(ts, message)| (ts.clone(), message.clone())),
                    );
                    let mut items: Vec<_> = stored
                        .into_values()
                        .filter(|message| {
                            message.thread_ts == thread
                                && !conversation.deleted.contains(&message.ts)
                        })
                        .filter(|message| {
                            anchor
                                .as_ref()
                                .is_none_or(|anchor| timestamp(&message.ts) < timestamp(anchor))
                        })
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
                    Ok((items, root, more, None, false))
                })
                .await
                .map_err(|_| Error::Unavailable)??
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
        let positions = &conversation.read_positions;
        let public_message = |message: &Message| conversation.message_result(message);
        let thread_items = thread
            .is_none()
            .then(|| conversation.cached_replies(&items, false));
        let items: Vec<_> = items.iter().map(public_message).collect();
        let mut result = json!({
            "api_version":"1","conversation_id":id,"items":items,"next_cursor":next,
            "sync_state":if live { "live" } else { "cached" },
            "read_through_ts":positions.get(thread.as_deref()),"covered_range":covered,
            "first_unread_ts":conversation.messages.values()
                .filter(|message| message.thread_ts == thread && positions.unread(message, account))
                .min_by_key(|message| timestamp(&message.ts)).map(|message| &message.ts)
        });
        if thread.is_some() {
            result["root"] = root.as_ref().map_or(Value::Null, public_message);
        } else {
            result["thread_items"] = json!(thread_items.unwrap_or_default());
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
        let (exists, observed) = {
            let state = self.state.lock().await;
            state.account(session)?;
            let room = state.cache.conversations.get(id);
            (room.is_some(), room.and_then(|room| room.journal.as_ref()).map(|journal| journal.observe()))
        };
        if !exists {
            self.refresh_directory(session).await?;
            if !self.state.lock().await.cache.conversations.contains_key(id) {
                return Err(Error::Conversation);
            }
        }
        if let Some(thread) = thread {
            return self
                .fetch_thread(session, id, thread, before, limit, observed)
                .await;
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
        self.warm_message_media(session, &items).await;
        self.store_page(session, id, None, &items, None, more, observed)
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
        observed: Option<(tokio::sync::watch::Receiver<u64>, u64)>,
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
        self.warm_message_media(session, &items).await;
        self.warm_message_media(session, std::slice::from_ref(&root))
            .await;
        self.store_page(
            session,
            id,
            Some(thread),
            &items,
            Some(&root),
            more,
            observed,
        )
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
        observed: Option<(tokio::sync::watch::Receiver<u64>, u64)>,
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
            let changed = match (conversation.journal.as_ref(), observed.as_ref()) {
                (Some(journal), Some(observed)) => journal.changed_since(observed),
                (Some(_), None) => None,
                _ => Some(Default::default()),
            };
            // 拉取期间的实时事件优先于 Slack 分页响应。
            let current = |ts: &str| changed.as_ref().is_some_and(|changed| !changed.contains(ts));
            if let (Some(first), Some(last)) = (items.first(), items.last()) {
                let min = timestamp(&first.ts);
                let max = timestamp(&last.ts);
                let vanished: Vec<_> = conversation
                    .messages
                    .values()
                    .filter(|message| {
                        message.thread_ts == view
                            && current(&message.ts)
                            && timestamp(&message.ts) >= min
                            && timestamp(&message.ts) <= max
                            && !items.iter().any(|current| current.ts == message.ts)
                    })
                    .map(|message| message.ts.clone())
                    .collect();
                for ts in vanished {
                    conversation.remove(&ts, view.as_deref());
                }
            }
            for message in items.into_iter().chain(root).filter(|message| current(&message.ts)) {
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
            let conversation = cache.conversations.get_mut(&id).ok_or(Error::Conversation)?;
            let existing = conversation.read_positions.get(thread.as_deref()).and_then(timestamp);
            if existing.is_none_or(|value| position > value) {
                let belongs = |message: &Message| message.thread_ts == thread;
                let known = conversation.messages.get(&target).map_or_else(
                    || {
                        conversation.disk.as_ref()
                            .and_then(|disk| disk.message(&id, thread.as_deref(), &target))
                            .as_ref()
                            .is_some_and(belongs)
                    },
                    belongs,
                );
                if conversation.deleted.contains(&target) || !known {
                    return Err(Error::ReadPosition);
                }
                let previous = conversation.read_positions.clone();
                conversation.read_positions.cursors
                    .insert(thread.clone().unwrap_or_default(), target);
                if let Some(disk) = &conversation.disk
                    && disk.save_room(conversation).is_err()
                {
                    conversation.read_positions = previous;
                    return Err(Error::ReadPositionStorage);
                }
            }
            let (_, _, summary) = conversation.summarize(account);
            Ok(json!({"api_version":"1","conversation_id":id,"thread_ts":thread,
                "read_through_ts":conversation.read_positions.get(thread.as_deref()),"unread_count":summary["unread_count"]}))
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
