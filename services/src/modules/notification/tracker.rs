use std::{
    collections::HashMap,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::Duration,
};

use tokio::time::Instant;
use uuid::Uuid;

use super::{
    protocol::{API_VERSION, NotificationReceipt, NotificationStatus, NotificationStatusResponse},
    service::NotificationError,
};
use crate::platform::macos::notification::NotificationContent;

const RETRY_INTERVAL: Duration = Duration::from_secs(60);
const CONFIRMED_RETENTION: Duration = Duration::from_secs(15 * 60);
const MAX_PENDING: usize = 1024;
const MAX_CONFIRMED: usize = 1024;

#[derive(Debug)]
struct Pending {
    content: NotificationContent,
    retry_at: Instant,
    failure: Option<(String, String)>,
    in_flight: Weak<()>,
}

#[derive(Debug)]
pub(super) struct DeliveryAttempt {
    pub content: NotificationContent,
    _in_flight: Arc<()>,
}

impl Pending {
    fn new(content: NotificationContent) -> Self {
        Self {
            content,
            retry_at: Instant::now(),
            failure: None,
            in_flight: Weak::new(),
        }
    }

    fn is_in_flight(&self) -> bool {
        self.in_flight.strong_count() != 0
    }

    fn begin_attempt(&mut self, now: Instant) -> DeliveryAttempt {
        // 转交完成、超时或取消时，DeliveryAttempt 释放占用。
        let in_flight = Arc::new(());
        self.in_flight = Arc::downgrade(&in_flight);
        self.retry_at = now + RETRY_INTERVAL;
        DeliveryAttempt {
            content: self.content.clone(),
            _in_flight: in_flight,
        }
    }
}

#[derive(Debug, Default)]
struct Records {
    pending: HashMap<Uuid, Pending>,
    confirmed: HashMap<Uuid, Instant>,
}

#[derive(Debug, Default)]
pub(super) struct NotificationTracker {
    directory: Option<PathBuf>,
    records: Records,
}

impl NotificationTracker {
    pub fn load(directory: PathBuf) -> io::Result<Self> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let mut records = Records::default();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                continue;
            };
            if path != cache_path(&directory, id) || !entry.file_type()?.is_file() {
                continue;
            }
            let content = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<NotificationContent>(&bytes).ok())
                .filter(|content| content.notification_id == id.to_string());
            if let Some(content) = content {
                records.pending.insert(id, Pending::new(content));
            } else {
                eprintln!("无法恢复待确认通知 {id}");
            }
        }
        Ok(Self {
            directory: Some(directory),
            records,
        })
    }

    pub fn register(
        &mut self,
        content: NotificationContent,
        id: Uuid,
    ) -> Result<DeliveryAttempt, NotificationError> {
        let records = &mut self.records;
        if records.pending.len() >= MAX_PENDING {
            return Err(NotificationError::Capacity);
        }
        if let Some(directory) = &self.directory {
            write_pending(directory, id, &content)?;
        }
        let mut pending = Pending::new(content);
        let attempt = pending.begin_attempt(Instant::now());
        records.pending.insert(id, pending);
        Ok(attempt)
    }

    pub fn schedule_retry(&mut self) {
        let records = &mut self.records;
        let now = Instant::now();
        for pending in records.pending.values_mut() {
            if !pending.is_in_flight() {
                pending.retry_at = pending.retry_at.min(now);
            }
        }
    }

    pub fn due(&self) -> Vec<Uuid> {
        let now = Instant::now();
        let records = &self.records;
        let mut due: Vec<_> = records
            .pending
            .iter()
            .filter(|(_, pending)| pending.retry_at <= now && !pending.is_in_flight())
            .map(|(id, pending)| (pending.retry_at, *id))
            .collect();
        due.sort_unstable();
        due.into_iter().map(|(_, id)| id).collect()
    }

    pub fn retry(&mut self, id: Uuid) -> Option<DeliveryAttempt> {
        let records = &mut self.records;
        let pending = records.pending.get_mut(&id)?;
        let now = Instant::now();
        (pending.retry_at <= now && !pending.is_in_flight()).then(|| pending.begin_attempt(now))
    }

    pub fn get(&mut self, id: &str) -> Result<NotificationStatusResponse, NotificationError> {
        let id = parse_id(id)?;
        let records = &mut self.records;
        records.prune_confirmed();
        records.response(id)
    }

    pub fn receive(
        &mut self,
        id: &str,
        receipt: NotificationReceipt,
    ) -> Result<NotificationStatusResponse, NotificationError> {
        if let NotificationReceipt::Failed { code, message } = &receipt
            && (code.trim().is_empty() || message.trim().is_empty())
        {
            return Err(NotificationError::InvalidRequest(
                "失败回执的 code 和 message 不能为空".to_owned(),
            ));
        }
        let id = parse_id(id)?;
        let records = &mut self.records;
        records.prune_confirmed();
        match receipt {
            NotificationReceipt::Submitted {} if records.pending.contains_key(&id) => {
                if let Some(directory) = &self.directory {
                    match fs::remove_file(cache_path(directory, id)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                records.pending.remove(&id);
                if records.confirmed.len() >= MAX_CONFIRMED
                    && let Some(oldest) = records
                        .confirmed
                        .iter()
                        .min_by_key(|(_, time)| **time)
                        .map(|(id, _)| *id)
                {
                    records.confirmed.remove(&oldest);
                }
                records.confirmed.insert(id, Instant::now());
            }
            NotificationReceipt::Failed { code, message } => {
                if records.confirmed.contains_key(&id) {
                    return Err(NotificationError::ReceiptConflict);
                }
                let pending = records
                    .pending
                    .get_mut(&id)
                    .ok_or(NotificationError::NotFound)?;
                pending.failure = Some((code, message));
            }
            NotificationReceipt::Submitted {} => {}
        }
        records.response(id)
    }
}

impl Records {
    fn prune_confirmed(&mut self) {
        self.confirmed
            .retain(|_, time| time.elapsed() < CONFIRMED_RETENTION);
    }

    fn response(&self, id: Uuid) -> Result<NotificationStatusResponse, NotificationError> {
        let (status, failure) = if let Some(pending) = self.pending.get(&id) {
            (NotificationStatus::Unconfirmed, pending.failure.as_ref())
        } else if self.confirmed.contains_key(&id) {
            (NotificationStatus::Confirmed, None)
        } else {
            return Err(NotificationError::NotFound);
        };
        Ok(NotificationStatusResponse {
            api_version: API_VERSION.to_owned(),
            notification_id: id.to_string(),
            status,
            code: failure.map(|(code, _)| code.clone()),
            message: failure.map(|(_, message)| message.clone()),
        })
    }
}

fn parse_id(id: &str) -> Result<Uuid, NotificationError> {
    Uuid::parse_str(id).map_err(|_| NotificationError::NotFound)
}

fn cache_path(directory: &Path, id: Uuid) -> PathBuf {
    directory.join(format!("{id}.json"))
}

fn write_pending(directory: &Path, id: Uuid, content: &NotificationContent) -> io::Result<()> {
    let temporary = directory.join(format!(".{id}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, content)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, cache_path(directory, id))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
