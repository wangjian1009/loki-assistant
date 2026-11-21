use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::{Error, Identity, Message, timestamp};

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct Secrets {
    pub app_token: Option<String>,
    pub account: Option<Account>,
    #[serde(default)]
    pub disabled_token: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Account {
    pub identity: Identity,
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: Option<u64>,
    pub baseline: u64,
    #[serde(default)]
    pub imported_token_hash: Option<String>,
    #[serde(default)]
    pub revoked: bool,
}

pub(super) trait Keychain: Send + Sync {
    fn load(&self) -> Result<Secrets, Error>;
    fn save(&self, secrets: &Secrets) -> Result<(), Error>;
}

pub(super) struct LoginKeychain(pub String);

impl Keychain for LoginKeychain {
    fn load(&self) -> Result<Secrets, Error> {
        #[cfg(target_os = "macos")]
        {
            let result = security_framework::passwords::get_generic_password(
                "cc.loki-assistant.slack",
                &self.0,
            );
            match result {
                Ok(data) => serde_json::from_slice(&data).map_err(|_| Error::Credentials),
                Err(error) if error.code() == -25300 => Ok(Secrets::default()),
                Err(_) => Err(Error::Credentials),
            }
        }
        #[cfg(not(target_os = "macos"))]
        Err(Error::Credentials)
    }

    fn save(&self, _secrets: &Secrets) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        {
            let data = serde_json::to_vec(_secrets).map_err(|_| Error::Credentials)?;
            security_framework::passwords::set_generic_password(
                "cc.loki-assistant.slack",
                &self.0,
                &data,
            )
            .map_err(|_| Error::Credentials)
        }
        #[cfg(not(target_os = "macos"))]
        Err(Error::Credentials)
    }
}

pub(super) fn random() -> Result<String, Error> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Storage)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn read_private(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other("权限或类型无效"));
    }
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(io::Error::other("文件过大"));
    }
    Ok(data)
}

fn write_private(path: &Path, data: &[u8], replace: bool) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("缺少数据目录"))?;
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".slack-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        if replace {
            fs::rename(&temporary, path)?;
        } else {
            fs::hard_link(&temporary, path)?;
        }
        File::open(directory)?.sync_all()
    })();
    let _ = fs::remove_file(temporary);
    result
}

pub(super) fn local_token(path: &Path) -> Result<String, Error> {
    match read_private(path, 64) {
        Ok(value) => {
            let value = String::from_utf8(value).map_err(|_| Error::Storage)?;
            if URL_SAFE_NO_PAD
                .decode(&value)
                .is_ok_and(|bytes| bytes.len() == 32)
            {
                Ok(value)
            } else {
                Err(Error::Storage)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let token = random()?;
            match write_private(path, token.as_bytes(), false) {
                Ok(()) => Ok(token),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => local_token(path),
                Err(_) => Err(Error::Storage),
            }
        }
        Err(_) => Err(Error::Storage),
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct Cache {
    pub session_id: Option<Uuid>,
    pub conversations: BTreeMap<String, Conversation>,
    pub directory_loaded: bool,
    pub sends: BTreeMap<Uuid, SendRecord>,
    pub events: BTreeSet<String>,
    #[serde(default)]
    pub notifications: BTreeSet<String>,
}

impl Cache {
    pub fn load(path: &Path) -> Result<Self, Error> {
        match read_private(path, 128 * 1024 * 1024) {
            Ok(data) => serde_json::from_slice(&data).map_err(|_| Error::Storage),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(Error::Storage),
        }
    }

    pub fn save(&self, path: Option<&PathBuf>) -> Result<(), Error> {
        if let Some(path) = path {
            let data = serde_json::to_vec(self).map_err(|_| Error::Storage)?;
            write_private(path, &data, true).map_err(|_| Error::Storage)?;
        }
        Ok(())
    }

    pub fn reset(&mut self, session: Option<Uuid>) {
        if self.session_id != session {
            *self = Self {
                session_id: session,
                ..Self::default()
            };
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Conversation {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub messages: BTreeMap<String, Message>,
    #[serde(default)]
    pub deleted: BTreeSet<String>,
    #[serde(default)]
    pub positions: BTreeMap<String, String>,
    #[serde(default)]
    pub loaded: BTreeMap<String, bool>,
}

impl Conversation {
    pub fn new(id: &str, kind: &str, title: &str) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            participants: Vec::new(),
            messages: BTreeMap::new(),
            deleted: BTreeSet::new(),
            positions: BTreeMap::new(),
            loaded: BTreeMap::new(),
        }
    }

    pub fn read_position(&self, thread: Option<&str>) -> Option<&str> {
        self.positions
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
            .read_position(message.thread_ts.as_deref())
            .and_then(timestamp)
            .unwrap_or((account.baseline, 0));
        timestamp(&message.ts).is_some_and(|value| value > position)
    }

    pub fn summarize(&self, account: &Account) -> (Option<(u64, u32)>, Option<(u64, u32)>, Value) {
        let latest = self
            .messages
            .values()
            .max_by_key(|message| timestamp(&message.ts));
        let mut unread: Vec<_> = self
            .messages
            .values()
            .filter(|message| self.unread(message, account))
            .collect();
        unread.sort_by_key(|message| timestamp(&message.ts));
        (
            latest.and_then(|message| timestamp(&message.ts)),
            unread.last().and_then(|message| timestamp(&message.ts)),
            json!({
                "id":self.id, "kind":self.kind, "title":self.title,
                "last_activity_ts":latest.map(|message| &message.ts),
                "preview":latest.map(|message| message.text.chars().take(120).collect::<String>()),
                "unread_count":unread.len(),
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
            .is_some_and(|old| old.edited && !message.edited)
        {
            return;
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

    pub fn remove(&mut self, ts: &str) {
        self.messages.remove(ts);
        self.deleted.insert(ts.into());
        if self.deleted.len() > 2_000 {
            self.deleted.pop_first();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SendState {
    Pending,
    Attempting,
    Sent,
    Failed,
    Unknown,
}

#[derive(Clone, Deserialize, Serialize)]
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
