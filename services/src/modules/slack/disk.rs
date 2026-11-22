use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use lexpr::Value as Sexp;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    mailbox::UserProfile,
    protocol::{Attachment, Message, timestamp, valid_id},
    store::Conversation,
};

#[derive(Clone)]
pub(super) struct DiskCache(PathBuf);

pub(super) struct PendingAttachment {
    temporary: PathBuf,
    destination: PathBuf,
}

impl PendingAttachment {
    pub fn commit(self) -> io::Result<PathBuf> {
        fs::rename(&self.temporary, &self.destination)?;
        Ok(self.destination.clone())
    }
}

impl Drop for PendingAttachment {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

#[derive(Deserialize, Serialize)]
struct RoomInfo {
    id: String,
    kind: String,
    title: String,
    participants: Vec<String>,
}

impl DiskCache {
    pub fn open(root: &Path, team: &str, user: &str) -> io::Result<Self> {
        private_dir(root)?;
        let cache = Self(root.join(team));
        private_dir(&cache.0)?;
        let owner = cache.0.join(".owner");
        match fs::read_to_string(&owner) {
            Ok(previous) if previous == user => return Ok(cache),
            Ok(_) => {
                for name in ["users", "conversations", "attachments"] {
                    match fs::remove_dir_all(cache.0.join(name)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        atomic_write(&owner, user.as_bytes())?;
        Ok(cache)
    }

    fn user_path(&self, id: &str, ext: &str) -> PathBuf {
        self.0.join("users").join(format!("{id}.{ext}"))
    }

    pub fn users(&self, team: &str) -> Vec<UserProfile> {
        let mut users: Vec<_> = entries(&self.0.join("users"), false)
            .filter_map(|path| {
                let id = txt_stem(&path).filter(|id| valid_id(id))?;
                let user: UserProfile = read_record(&path)?;
                (user.id == format!("{team}.{id}")).then_some(user)
            })
            .collect();
        users.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        users
    }

    pub fn save_user(&self, profile: &UserProfile, previous: Option<&UserProfile>) {
        let id = profile.slack_id();
        if profile.avatar_url.is_none()
            || previous.is_some_and(|previous| previous.avatar_url != profile.avatar_url)
        {
            let _ = fs::remove_file(self.user_path(id, "png"));
        }
        save_record(&self.user_path(id, "txt"), profile);
    }

    pub fn remove_user(&self, id: &str) {
        for ext in ["txt", "png"] {
            let _ = fs::remove_file(self.user_path(id, ext));
        }
    }

    pub fn user_result(&self, user: &UserProfile) -> Value {
        let mut value = json!(user);
        add_path(
            &mut value,
            "cache_path",
            self.user_path(user.slack_id(), "txt"),
        );
        add_path(
            &mut value,
            "avatar_path",
            self.user_path(user.slack_id(), "png"),
        );
        value
    }

    pub fn avatar_path(&self, id: &str) -> PathBuf {
        self.user_path(id, "png")
    }

    pub fn room_path(&self, id: &str) -> PathBuf {
        self.0.join("conversations").join(id).join("info.txt")
    }

    pub fn save_room(&self, room: &Conversation) {
        save_record(
            &self.room_path(&room.id),
            &RoomInfo {
                id: room.id.clone(),
                kind: room.kind.clone(),
                title: room.title.clone(),
                participants: room.participants.clone(),
            },
        );
    }

    pub fn remove_room(&self, id: &str) {
        let _ = fs::remove_dir_all(self.0.join("conversations").join(id));
    }

    pub fn rooms(&self) -> BTreeMap<String, Conversation> {
        entries(&self.0.join("conversations"), true)
            .filter_map(|path| {
                let id = path.file_name()?.to_str().filter(|id| valid_id(id))?;
                let info: RoomInfo = read_record(&path.join("info.txt"))?;
                if info.id != id {
                    return None;
                }
                let mut room = Conversation::new(id, &info.kind, &info.title);
                room.participants = info.participants;
                let mut paths: Vec<_> = message_paths(&path.join("messages")).collect();
                for thread in entries(&path.join("threads"), true) {
                    paths.extend(message_paths(&thread));
                }
                paths.sort_unstable_by_key(|path| txt_stem(path).and_then(timestamp));
                for path in paths.into_iter().rev().take(1000) {
                    if let Some(message) = read_message(&path) {
                        room.messages.insert(message.ts.clone(), message);
                    }
                }
                room.disk = Some(self.clone());
                Some((id.into(), room))
            })
            .collect()
    }

    fn message_path(&self, id: &str, thread: Option<&str>, ts: &str) -> PathBuf {
        let room = self.0.join("conversations").join(id);
        let view = match thread {
            Some(thread) => room.join("threads").join(thread),
            None => room.join("messages"),
        };
        view.join(format!("{ts}.txt"))
    }

    pub fn message(&self, id: &str, thread: Option<&str>, ts: &str) -> Option<Message> {
        read_message(&self.message_path(id, thread, ts))
    }

    pub fn find_message(&self, id: &str, ts: &str) -> Option<Message> {
        self.message(id, None, ts).or_else(|| {
            let threads = self.0.join("conversations").join(id).join("threads");
            entries(&threads, true)
                .find_map(|thread| read_message(&thread.join(format!("{ts}.txt"))))
        })
    }

    pub fn messages(&self, id: &str, thread: Option<&str>) -> impl Iterator<Item = Message> {
        let path = self.message_path(id, thread, "_");
        message_paths(path.parent().expect("Slack 消息目录")).filter_map(|path| read_message(&path))
    }

    pub fn save_message(&self, id: &str, message: &Message) {
        let mut value = json!(message);
        if let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) {
            for (value, item) in attachments.iter_mut().zip(&message.attachments) {
                value["file_id"] = json!(item.file_id);
                value["filename"] = json!(item.filename);
                value["image_url"] = json!(item.image_url);
                value["download_url"] = json!(item.download_url);
            }
        }
        save_record(
            &self.message_path(id, message.thread_ts.as_deref(), &message.ts),
            &value,
        );
    }

    pub fn remove_message(&self, id: &str, ts: &str, thread: Option<&str>) {
        let _ = fs::remove_file(self.message_path(id, thread, ts));
        if thread.is_none() {
            let threads = self.0.join("conversations").join(id).join("threads");
            for path in entries(&threads, true) {
                let _ = fs::remove_file(path.join(format!("{ts}.txt")));
            }
        }
    }

    pub fn message_result(&self, id: &str, message: &Message, mut value: Value) -> Value {
        add_path(
            &mut value,
            "cache_path",
            self.message_path(id, message.thread_ts.as_deref(), &message.ts),
        );
        if let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) {
            for (value, item) in attachments.iter_mut().zip(&message.attachments) {
                if let Some(path) = self.attachment(item, true) {
                    add_path(value, "preview_path", path);
                }
                if let Some(path) = self.attachment(item, false) {
                    add_path(value, "original_path", path);
                }
            }
        }
        value
    }

    fn attachment_base(item: &Attachment, preview: bool) -> Option<String> {
        let url = if preview {
            &item.image_url
        } else {
            &item.download_url
        };
        let url = url.as_ref()?;
        let key = item
            .file_id
            .clone()
            .unwrap_or_else(|| format!("url-{:x}", Sha256::digest(url.as_bytes())));
        Some(format!(
            "{key}.{}",
            if preview { "preview" } else { "original" }
        ))
    }

    pub fn attachment(&self, item: &Attachment, preview: bool) -> Option<PathBuf> {
        let prefix = format!("{}.", Self::attachment_base(item, preview)?);
        let dir = self.0.join("attachments");
        if preview {
            return ["png", "jpg", "gif", "webp"]
                .into_iter()
                .map(|ext| dir.join(format!("{prefix}{ext}")))
                .find(|path| regular_file(path));
        }
        if let Some(ext) = original_extension(item) {
            let path = dir.join(format!("{prefix}{ext}"));
            if regular_file(&path) {
                return Some(path);
            }
        }
        let ext = item
            .mimetype
            .as_deref()
            .and_then(mime_extension)
            .unwrap_or("bin");
        let path = dir.join(format!("{prefix}{ext}"));
        if regular_file(&path) {
            return Some(path);
        }
        if original_extension(item).is_some() {
            return None;
        }
        entries(&dir, false).find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.strip_prefix(&prefix).is_some_and(valid_extension))
        })
    }

    fn attachment_destination(
        &self,
        item: &Attachment,
        preview: bool,
        mime: &str,
    ) -> Option<PathBuf> {
        let base = Self::attachment_base(item, preview)?;
        let ext = if preview {
            mime_extension(mime)?
        } else {
            original_extension(item)
                .or_else(|| mime_extension(mime))
                .unwrap_or("bin")
        };
        Some(self.0.join("attachments").join(format!("{base}.{ext}")))
    }

    pub fn begin_original(
        &self,
        item: &Attachment,
        mime: &str,
    ) -> io::Result<(PendingAttachment, fs::File)> {
        let destination = self
            .attachment_destination(item, false, mime)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let parent = destination.parent().ok_or(io::ErrorKind::InvalidInput)?;
        private_dir(parent)?;
        let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        Ok((
            PendingAttachment {
                temporary,
                destination,
            },
            file,
        ))
    }

    pub fn save_preview(&self, item: &Attachment, mime: &str, bytes: &[u8]) -> Option<PathBuf> {
        let path = self.attachment_destination(item, true, mime)?;
        atomic_write(&path, bytes).ok()?;
        Some(path)
    }

    pub fn clean(root: &Path, retention: Duration) {
        let now = SystemTime::now();
        let expired = |path: &Path| {
            fs::symlink_metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > retention)
        };
        for team in entries(root, true).filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(super::projects::valid_team_id)
                && regular_file(&path.join(".owner"))
        }) {
            for path in entries(&team.join("attachments"), false) {
                if expired(&path) {
                    let _ = fs::remove_file(path);
                }
            }
            for room in entries(&team.join("conversations"), true) {
                let messages = room.join("messages");
                let threads = room.join("threads");
                for thread in entries(&threads, true) {
                    let Some(ts) = thread
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|ts| timestamp(ts).is_some())
                    else {
                        continue;
                    };
                    let parent = messages.join(format!("{ts}.txt"));
                    if (!regular_file(&parent) || expired(&parent))
                        && message_paths(&thread).all(|path| expired(&path))
                        && fs::remove_dir_all(&thread).is_ok()
                    {
                        let _ = fs::remove_file(parent);
                    }
                }
                for path in message_paths(&messages) {
                    if expired(&path) && !threads.join(txt_stem(&path).unwrap_or_default()).is_dir()
                    {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }
}

pub(super) fn avatar_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = image::ImageReader::new(io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let mut output = io::Cursor::new(Vec::new());
    image.write_to(&mut output, image::ImageFormat::Png).ok()?;
    Some(output.into_inner())
}

pub(super) fn add_path(value: &mut Value, field: &str, path: PathBuf) {
    if regular_file(&path) {
        value[field] = json!(path);
    }
}

pub(super) fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    private_dir(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn entries(path: &Path, directories: bool) -> impl Iterator<Item = PathBuf> + use<> {
    // 工作区缓存目录只使用普通文件和目录。
    fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_dir())
        .and_then(|_| fs::read_dir(path).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(move |entry| {
            entry.file_type().is_ok_and(|kind| {
                if directories {
                    kind.is_dir()
                } else {
                    kind.is_file()
                }
            })
        })
        .map(|entry| entry.path())
}

fn txt_stem(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.strip_suffix(".txt")
}

fn message_paths(path: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    entries(path, false).filter(|path| txt_stem(path).and_then(timestamp).is_some())
}

fn read_message(path: &Path) -> Option<Message> {
    let value: Value = read_record(path)?;
    let mut message: Message = serde_json::from_value(value.clone()).ok()?;
    let root = path.parent()?.file_name()?.to_str()?;
    let thread = (root != "messages").then_some(root);
    if txt_stem(path)? != message.ts || message.thread_ts.as_deref() != thread {
        return None;
    }
    if let Some(attachments) = value["attachments"].as_array() {
        for (item, value) in message.attachments.iter_mut().zip(attachments) {
            item.file_id = value["file_id"]
                .as_str()
                .filter(|id| valid_id(id))
                .map(str::to_owned);
            item.filename = value["filename"].as_str().map(str::to_owned);
            item.image_url = value["image_url"].as_str().map(str::to_owned);
            item.download_url = value["download_url"].as_str().map(str::to_owned);
        }
    }
    Some(message)
}

fn save_record(path: &Path, value: &impl Serialize) {
    if let Ok(value) = serde_json::to_value(value)
        && let Ok(mut text) =
            lexpr::to_string_custom(&to_sexp(&value), lexpr::print::Options::elisp())
    {
        text.push('\n');
        if !regular_file(path) || !fs::read(path).is_ok_and(|old| old == text.as_bytes()) {
            let _ = atomic_write(path, text.as_bytes());
        }
    }
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Option<T> {
    if !regular_file(path) {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let data = lexpr::from_str_custom(&text, lexpr::parse::Options::elisp()).ok()?;
    serde_json::from_value(from_sexp(&data)?).ok()
}

fn to_sexp(value: &Value) -> Sexp {
    match value {
        Value::Null => Sexp::keyword("null"),
        Value::Bool(false) => Sexp::keyword("false"),
        Value::Bool(true) => Sexp::symbol("t"),
        Value::Number(number) => {
            if let Some(number) = number.as_u64() {
                Sexp::from(number)
            } else if let Some(number) = number.as_i64() {
                Sexp::from(number)
            } else {
                Sexp::from(number.as_f64().unwrap_or_default())
            }
        }
        Value::String(text) => Sexp::string(text.as_str()),
        Value::Array(items) => Sexp::vector(items.iter().map(to_sexp).collect::<Vec<_>>()),
        Value::Object(fields) => Sexp::list(
            fields
                .iter()
                .map(|(key, value)| Sexp::cons(Sexp::symbol(key.as_str()), to_sexp(value))),
        ),
    }
}

fn from_sexp(value: &Sexp) -> Option<Value> {
    Some(match value {
        Sexp::Keyword(key) if key.as_ref() == "null" => Value::Null,
        Sexp::Keyword(key) if key.as_ref() == "false" => Value::Bool(false),
        Sexp::Symbol(symbol) if symbol.as_ref() == "t" => Value::Bool(true),
        Sexp::String(text) => json!(text),
        Sexp::Number(number) => {
            if let Some(number) = number.as_u64() {
                json!(number)
            } else if let Some(number) = number.as_i64() {
                json!(number)
            } else {
                json!(number.as_f64()?)
            }
        }
        Sexp::Vector(items) => Value::Array(items.iter().map(from_sexp).collect::<Option<_>>()?),
        Sexp::Cons(_) | Sexp::Null => Value::Object(
            value
                .list_iter()?
                .map(|item| {
                    let (key, value) = item.as_pair()?;
                    Some((key.as_symbol()?.to_owned(), from_sexp(value)?))
                })
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

pub(super) fn mime_extension(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/json" => "json",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "audio/mpeg" => "mp3",
        "video/mp4" => "mp4",
        _ => return None,
    })
}

pub(super) fn preview_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn valid_extension(ext: &str) -> bool {
    (1..=16).contains(&ext.len()) && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn original_extension(item: &Attachment) -> Option<&str> {
    Path::new(item.filename.as_deref()?)
        .extension()?
        .to_str()
        .filter(|ext| valid_extension(ext))
}
