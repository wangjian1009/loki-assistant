use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Arc,
};

use reqwest::{
    Response, StatusCode,
    header::{CONTENT_ENCODING, CONTENT_RANGE, ETAG},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore},
};
use uuid::Uuid;

use super::{
    client::attachment_mime,
    disk::{DiskCache, PartialAttachment, PartialMetadata},
    projects::Projects,
    protocol::{Attachment, Error},
    service::SlackService,
};

pub(super) struct Downloads {
    registry: Mutex<Registry>,
    slots: Semaphore,
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            registry: Mutex::default(),
            slots: Semaphore::new(4),
        }
    }
}

#[derive(Default)]
struct Registry {
    tasks: HashMap<Uuid, Arc<Task>>,
    active: HashMap<PathBuf, (Arc<Task>, Arc<Mutex<()>>)>,
}

struct Task {
    id: Uuid,
    team: String,
    session: Uuid,
    progress: Mutex<Progress>,
}

#[derive(Default)]
struct Progress {
    bytes: u64,
    total: Option<u64>,
    state: DownloadState,
}

#[derive(Default)]
enum DownloadState {
    #[default]
    Pending,
    Downloading,
    Completed(PathBuf),
    Failed(Error),
}

impl Progress {
    fn complete(&mut self, path: PathBuf, bytes: u64) {
        self.bytes = bytes;
        self.total = Some(bytes);
        self.state = DownloadState::Completed(path);
    }

    fn result(&self, id: Uuid) -> Value {
        let state = match self.state {
            DownloadState::Pending => "pending",
            DownloadState::Downloading => "downloading",
            DownloadState::Completed(_) => "completed",
            DownloadState::Failed(_) => "failed",
        };
        let mut result = json!({
            "download_id": id,
            "state": state,
            "downloaded_bytes": self.bytes,
            "total_bytes": self.total,
        });
        match &self.state {
            DownloadState::Completed(path) => result["cache_path"] = json!(path),
            DownloadState::Failed(error) => result["error"] = error.object(),
            _ => {}
        }
        result
    }
}

impl Projects {
    pub(super) async fn start_download(
        &self,
        team: &str,
        service: Arc<SlackService>,
        session: Uuid,
        conversation: &str,
        ts: &str,
        index: usize,
    ) -> Result<(Uuid, bool), Error> {
        let state = service.state.lock().await;
        let account = state.account(session)?;
        let item = state.message_attachment(conversation, ts, index)?;
        let cache = state.cache.disk.clone().ok_or(Error::Unavailable)?;
        let partial = cache
            .partial_attachment(&item, &account.identity.user_id)
            .ok_or(Error::Attachment)?;
        let mut registry = self.downloads.registry.lock().await;
        let existing = registry.active.get(&partial.path);
        if let Some((task, _)) = existing
            && task.session == session
        {
            return Ok((task.id, false));
        }
        let file_lock = existing.map_or_else(|| Arc::new(Mutex::new(())), |(_, lock)| lock.clone());
        let mut progress = Progress::default();
        let cached = cache.attachment(&item, false).and_then(|path| {
            let size = fs::symlink_metadata(&path).ok()?.len();
            Some((path, size))
        });
        let complete = cached.is_some();
        if let Some((path, size)) = cached {
            progress.complete(path, size);
        }
        let task = Arc::new(Task {
            id: Uuid::new_v4(),
            team: team.to_owned(),
            session,
            progress: Mutex::new(progress),
        });
        registry.tasks.insert(task.id, task.clone());
        let task_id = task.id;
        if !complete {
            registry
                .active
                .insert(partial.path.clone(), (task.clone(), file_lock.clone()));
            let downloads = self.downloads.clone();
            drop(state);
            tokio::spawn(async move {
                let _file = file_lock.lock().await;
                let original = Original {
                    item,
                    cache,
                    partial,
                };
                let result = downloads.transfer(&service, &task, &original).await;
                let mut registry = downloads.registry.lock().await;
                let mut progress = task.progress.lock().await;
                match result {
                    Ok((path, bytes)) => progress.complete(path, bytes),
                    Err(error) => progress.state = DownloadState::Failed(error),
                }
                if registry
                    .active
                    .get(&original.partial.path)
                    .is_some_and(|(active, _)| active.id == task.id)
                {
                    registry.active.remove(&original.partial.path);
                }
            });
        }
        Ok((task_id, complete))
    }

    pub(super) async fn download_status(
        &self,
        id: Uuid,
        session: Option<Uuid>,
    ) -> Result<Value, Error> {
        let task = self
            .downloads
            .registry
            .lock()
            .await
            .tasks
            .get(&id)
            .cloned()
            .ok_or(Error::Download)?;
        if session.is_some_and(|session| session != task.session) {
            return Err(Error::Session);
        }
        let service = self.service(&task.team).await.map_err(|_| Error::Session)?;
        let state = service.state.lock().await;
        state.account(task.session).map_err(|_| Error::Session)?;
        Ok(task.progress.lock().await.result(id))
    }
}

impl Downloads {
    pub(super) async fn active_paths(&self) -> HashSet<PathBuf> {
        self.registry.lock().await.active.keys().cloned().collect()
    }

    async fn transfer(
        &self,
        service: &SlackService,
        task: &Task,
        original: &Original,
    ) -> Result<(PathBuf, u64), Error> {
        let _slot = self.slots.acquire().await.map_err(|_| Error::Unavailable)?;
        let result = original.transfer(service, task).await;
        if result.is_err() {
            let state = service.state.lock().await;
            if state.account(task.session).is_ok() && original.partial.read().is_none() {
                original.partial.discard();
            }
        }
        result
    }
}

struct Original {
    item: Attachment,
    cache: DiskCache,
    partial: PartialAttachment,
}

impl Original {
    async fn transfer(&self, service: &SlackService, task: &Task) -> Result<(PathBuf, u64), Error> {
        let url = self.item.download_url.as_deref().ok_or(Error::Attachment)?;
        let source = format!("{:x}", Sha256::digest(url.as_bytes()));
        let mut changes = service.change.subscribe();
        let token = service
            .state
            .lock()
            .await
            .account(task.session)?
            .access
            .clone();
        let web = service.web_session(task.session).await?;
        let mut resume = self.partial.read().filter(|(offset, metadata)| {
            metadata.source == source
                && strong_etag(&metadata.etag)
                && metadata.total.is_none_or(|total| *offset < total)
        });
        task.progress.lock().await.state = DownloadState::Downloading;
        loop {
            let mut response = service
                .client
                .transfer_original(
                    url,
                    &token,
                    web.as_ref(),
                    resume
                        .as_ref()
                        .map(|(offset, data)| (*offset, data.etag.as_str())),
                )
                .await?;
            let mut offset = 0;
            let mut limit = response.content_length();
            let mut total = limit;
            let metadata;
            let mime;
            let file = {
                let state = service.state.lock().await;
                state.account(task.session)?;
                let file = match response.status() {
                    StatusCode::OK if identity_encoding(&response) => {
                        total = total.or_else(|| self.item.file_id.as_ref().and(self.item.size));
                        limit = total;
                        mime = attachment_mime(&response).to_owned();
                        metadata = response
                            .headers()
                            .get(ETAG)
                            .and_then(|value| value.to_str().ok())
                            .filter(|value| strong_etag(value))
                            .map(|etag| PartialMetadata {
                                etag: etag.to_owned(),
                                source: source.clone(),
                                total,
                                mime: mime.clone(),
                            });
                        self.partial.reset()
                    }
                    StatusCode::PARTIAL_CONTENT => {
                        let segment = resume
                            .as_ref()
                            .and_then(|(offset, data)| response_range(&response, *offset, data));
                        let Some((end, length)) = segment else {
                            if resume.take().is_some() {
                                self.partial.discard();
                                continue;
                            }
                            return Err(Error::Unavailable);
                        };
                        let (start, mut data) = resume.take().expect("已验证续传范围");
                        offset = start;
                        limit = Some(end + 1);
                        total = Some(length);
                        data.total = total;
                        mime = data.mime.clone();
                        metadata = Some(data);
                        self.partial.append()
                    }
                    StatusCode::RANGE_NOT_SATISFIABLE if resume.take().is_some() => {
                        self.partial.discard();
                        continue;
                    }
                    status if status.is_server_error() => return Err(Error::Unavailable),
                    _ => return Err(Error::Attachment),
                }
                .map_err(|_| Error::Unavailable)?;
                if let Some(metadata) = &metadata {
                    self.partial
                        .save(metadata)
                        .map_err(|_| Error::Unavailable)?;
                }
                file
            };
            let mut file = tokio::fs::File::from_std(file);
            {
                let mut progress = task.progress.lock().await;
                progress.bytes = offset;
                progress.total = total;
            }
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
                if changes.has_changed().unwrap_or(true) {
                    changes.borrow_and_update();
                    service.state.lock().await.account(task.session)?;
                }
                let next = offset
                    .checked_add(chunk.len() as u64)
                    .ok_or(Error::Unavailable)?;
                if limit.is_some_and(|limit| next > limit) {
                    return Err(Error::Unavailable);
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|_| Error::Unavailable)?;
                file.flush().await.map_err(|_| Error::Unavailable)?;
                offset = next;
                task.progress.lock().await.bytes = offset;
            }
            drop(file);
            if limit.is_some_and(|limit| offset != limit) {
                return Err(Error::Unavailable);
            }
            if total.is_some_and(|total| offset < total) {
                resume = metadata.map(|metadata| (offset, metadata));
                continue;
            }
            let state = service.state.lock().await;
            state.account(task.session)?;
            let path = self
                .cache
                .attachment_destination(&self.item, false, &mime)
                .ok_or(Error::Unavailable)?;
            return self
                .partial
                .commit(path)
                .map(|path| (path, offset))
                .map_err(|_| Error::Unavailable);
        }
    }
}

fn identity_encoding(response: &Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .is_none_or(|value| value.as_bytes().eq_ignore_ascii_case(b"identity"))
}

fn strong_etag(value: &str) -> bool {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .is_some_and(|value| {
            value
                .bytes()
                .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte))
        })
}

fn response_range(
    response: &Response,
    offset: u64,
    metadata: &PartialMetadata,
) -> Option<(u64, u64)> {
    if !identity_encoding(response)
        || response
            .headers()
            .get(ETAG)
            .is_some_and(|value| value.as_bytes() != metadata.etag.as_bytes())
    {
        return None;
    }
    let (range, total) = response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes ")?
        .split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    (start == offset
        && end >= start
        && end < total
        && metadata.total.is_none_or(|known| known == total)
        && response
            .content_length()
            .is_none_or(|length| length == end - start + 1))
    .then_some((end, total))
}
