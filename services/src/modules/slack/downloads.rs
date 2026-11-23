use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use reqwest::{
    Response, StatusCode,
    header::{CONTENT_ENCODING, CONTENT_RANGE, ETAG},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::AsyncWriteExt,
    sync::{Semaphore, oneshot, watch},
};
use uuid::Uuid;

use crate::process::Process;

use super::{
    client::attachment_mime,
    disk::{CacheWrites, DiskCache, PartialAttachment, PartialMetadata},
    mailbox::UserProfile,
    projects::Projects,
    protocol::{Attachment, Error},
    service::SlackService,
    store::Conversation,
    team::Team,
};

pub(super) struct OpenedCache {
    pub disk: DiskCache,
    pub rooms: std::collections::BTreeMap<String, Conversation>,
    pub users: Vec<UserProfile>,
}

#[cfg(test)]
type TestPause = (oneshot::Sender<()>, oneshot::Receiver<()>);

pub(super) struct Downloads {
    registry: Process<Registry>,
    #[cfg(test)]
    pub(super) pause_probe: std::sync::Mutex<Option<TestPause>>,
    #[cfg(test)]
    pub(super) pause_cleanup: std::sync::Mutex<Option<TestPause>>,
}

impl Default for Downloads {
    fn default() -> Self {
        Self {
            registry: Process::start(Registry::default()),
            #[cfg(test)]
            pause_probe: Default::default(),
            #[cfg(test)]
            pause_cleanup: Default::default(),
        }
    }
}

#[derive(Default)]
struct Registry {
    tasks: HashMap<Uuid, Arc<Task>>,
    active: HashMap<PathBuf, (Arc<Task>, Arc<Semaphore>)>,
    teams: HashMap<String, TeamWork>,
}

impl Registry {
    fn cancel_team(&self, team: &str) {
        if let Some(work) = self.teams.get(team) {
            work.generation.send_replace(());
        }
    }
}

#[derive(Clone)]
struct TeamWork {
    directories: Arc<Semaphore>,
    partial_cleanup: Arc<Semaphore>,
    writes: CacheWrites,
    slots: Arc<Semaphore>,
    media_slots: Arc<Semaphore>,
    generation: watch::Sender<()>,
}

impl Default for TeamWork {
    fn default() -> Self {
        Self {
            directories: Arc::new(Semaphore::new(1)),
            partial_cleanup: Arc::new(Semaphore::new(1)),
            writes: CacheWrites::default(),
            slots: Arc::new(Semaphore::new(4)),
            media_slots: Arc::new(Semaphore::new(4)),
            generation: watch::channel(()).0,
        }
    }
}

struct Task {
    id: Uuid,
    team: String,
    session: Uuid,
    cancellation: watch::Receiver<()>,
    progress: Process<Progress>,
    completed: watch::Sender<Option<Result<PathBuf, Error>>>,
    registered: watch::Sender<Option<Result<bool, Error>>>,
}

impl Task {
    fn check_cancelled(&self) -> Result<(), Error> {
        if self.cancellation.has_changed().unwrap_or(true) {
            return Err(Error::DownloadCancelled);
        }
        Ok(())
    }

    async fn wait<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        let mut cancellation = self.cancellation.clone();
        tokio::select! {
            biased;
            _ = cancellation.changed() => Err(Error::DownloadCancelled),
            result = operation => result,
        }
    }
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
        service: Arc<Team>,
        session: Uuid,
        conversation: &str,
        ts: &str,
        index: usize,
    ) -> Result<(Uuid, bool), Error> {
        let (conversation, ts) = (conversation.to_owned(), ts.to_owned());
        service
            .request(move |service| {
                Box::pin(async move {
                    service
                        .downloads
                        .clone()
                        .start(service, session, conversation, ts, index)
                        .await
                })
            })
            .await
    }

    pub(super) async fn original_path(&self, id: Uuid) -> Result<PathBuf, Error> {
        let task = self
            .downloads
            .registry
            .call(move |registry| registry.tasks.get(&id).cloned())
            .await
            .ok_or(Error::Download)?;
        let mut completed = task.completed.subscribe();
        let result = completed
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Error::Unavailable)?;
        result.clone().expect("下载已完成")
    }

    pub(super) async fn download_status(
        &self,
        id: Uuid,
        session: Option<Uuid>,
    ) -> Result<Value, Error> {
        let task = self
            .downloads
            .registry
            .call(move |registry| registry.tasks.get(&id).cloned())
            .await
            .ok_or(Error::Download)?;
        if session.is_some_and(|session| session != task.session) {
            return Err(Error::Session);
        }
        let service = self.service(&task.team).await.map_err(|_| Error::Session)?;
        service
            .check_session(task.session)
            .await
            .map_err(|_| Error::Session)?;
        Ok(task
            .progress
            .call(move |progress| progress.result(id))
            .await)
    }
}

impl Downloads {
    async fn work(&self, team: &str) -> TeamWork {
        let team = team.to_owned();
        self.registry
            .call(move |registry| registry.teams.entry(team).or_default().clone())
            .await
    }

    pub(super) fn request_cancel_team(&self, team: &str) {
        let team = team.to_owned();
        self.registry
            .post(move |registry| registry.cancel_team(&team));
    }

    pub(super) async fn cancel_team(&self, team: &str) {
        let team = team.to_owned();
        self.registry
            .call(move |registry| registry.cancel_team(&team))
            .await;
    }

    pub(super) async fn cached_teams(&self, root: PathBuf) -> Vec<String> {
        let (reply, response) = oneshot::channel();
        self.registry.post(move |_| {
            tokio::task::spawn_blocking(move || {
                let _ = reply.send(DiskCache::cached_teams(&root));
            });
        });
        response.await.unwrap_or_default()
    }

    pub(super) async fn open_cache(
        &self,
        root: PathBuf,
        team: String,
        user: String,
        cancelled: Arc<AtomicBool>,
    ) -> std::io::Result<OpenedCache> {
        let (reply, response) = oneshot::channel();
        self.registry.post(move |registry| {
            let work = registry.teams.entry(team.clone()).or_default().clone();
            tokio::spawn(async move {
                let _directory = work
                    .directories
                    .acquire()
                    .await
                    .expect("缓存目录许可不能关闭");
                let result = tokio::task::spawn_blocking(move || {
                    let disk =
                        DiskCache::open_with_writes(&root, &team, &user, work.writes, || {
                            cancelled.load(Ordering::Acquire)
                        })?;
                    Ok(OpenedCache {
                        rooms: disk.rooms(),
                        users: disk.users(&team),
                        disk,
                    })
                })
                .await
                .map_err(std::io::Error::other)
                .and_then(|result| result);
                let _ = reply.send(result);
            });
        });
        response.await.map_err(std::io::Error::other)?
    }

    pub(super) async fn media<T: Send + 'static>(
        &self,
        team: &str,
        operation: impl std::future::Future<Output = Result<T, Error>> + Send + 'static,
    ) -> Result<(T, watch::Receiver<()>), Error> {
        let (reply, response) = oneshot::channel();
        let team = team.to_owned();
        self.registry.post(move |registry| {
            let work = registry.teams.entry(team).or_default().clone();
            let cancellation = work.generation.subscribe();
            tokio::spawn(async move {
                let mut changed = cancellation.clone();
                let result = tokio::select! {
                    biased;
                    _ = changed.changed() => Err(Error::DownloadCancelled),
                    result = async {
                        let _slot = work.media_slots.acquire().await.map_err(|_| Error::Unavailable)?;
                        operation.await
                    } => result,
                };
                let _ = reply.send(result.map(|value| (value, cancellation)));
            });
        });
        response.await.map_err(|_| Error::Unavailable)?
    }

    async fn start(
        self: Arc<Self>,
        service: Arc<SlackService>,
        session: Uuid,
        conversation: String,
        ts: String,
        index: usize,
    ) -> Result<(Uuid, bool), Error> {
        let team = service.project.0.clone();
        let work = self.work(&team).await;
        let cancellation = work.generation.subscribe();
        let (item, cache, partial) = service
            .state
            .blocking(move |state| {
                let account = state.account(session)?;
                let item = state.message_attachment(&conversation, &ts, index)?;
                let cache = state.cache.disk.clone().ok_or(Error::Unavailable)?;
                let partial = cache
                    .partial_attachment(&item, &account.identity.user_id)
                    .ok_or(Error::Attachment)?;
                Ok((item, cache, partial))
            })
            .await?;
        let downloads = self.clone();
        let task = self
            .registry
            .call(move |registry| {
                if cancellation.has_changed().unwrap_or(true) {
                    return Err(Error::DownloadCancelled);
                }
                let existing = registry.active.get(&partial.path);
                if let Some((task, _)) = existing
                    && task.session == session
                    && task.check_cancelled().is_ok()
                    && task.completed.borrow().is_none()
                {
                    return Ok(task.clone());
                }
                let file_lock =
                    existing.map_or_else(|| Arc::new(Semaphore::new(1)), |(_, lock)| lock.clone());
                let task = Arc::new(Task {
                    id: Uuid::new_v4(),
                    team,
                    session,
                    cancellation,
                    progress: Process::start(Progress::default()),
                    completed: watch::channel(None).0,
                    registered: watch::channel(None).0,
                });
                registry.tasks.insert(task.id, task.clone());
                registry
                    .active
                    .insert(partial.path.clone(), (task.clone(), file_lock.clone()));
                let running = task.clone();
                tokio::spawn(async move {
                    let task = running;
                    let original = Original {
                        item,
                        cache,
                        partial,
                    };
                    let result = async {
                        let _file = task
                            .wait(async {
                                file_lock.acquire().await.map_err(|_| Error::Unavailable)
                            })
                            .await?;
                        let probe = original.cache.clone();
                        let item = original.item.clone();
                        #[cfg(test)]
                        let test_pause = downloads.pause_probe.lock().unwrap().take();
                        let cached = tokio::task::spawn_blocking(move || {
                            #[cfg(test)]
                            if let Some((entered, release)) = test_pause {
                                let _ = entered.send(());
                                let _ = release.blocking_recv();
                            }
                            probe.attachment(&item, false).and_then(|path| {
                                let size = fs::symlink_metadata(&path).ok()?.len();
                                Some((path, size))
                            })
                        })
                        .await
                        .map_err(|_| Error::Unavailable)?;
                        task.check_cancelled()?;
                        if let Some(cached) = cached {
                            return Ok(cached);
                        }
                        task.registered.send_replace(Some(Ok(false)));
                        let _slot = task
                            .wait(async {
                                work.slots.acquire().await.map_err(|_| Error::Unavailable)
                            })
                            .await?;
                        // 清理对半成品的快照在线程开始写之前完成。
                        let partial_ready = task
                            .wait(async {
                                work.partial_cleanup
                                    .acquire()
                                    .await
                                    .map_err(|_| Error::Unavailable)
                            })
                            .await?;
                        drop(partial_ready);
                        downloads.transfer(&service, &task, &original).await
                    }
                    .await;
                    let completed = result.clone().map(|(path, _)| path);
                    let registered = result.as_ref().map(|_| true).map_err(|error| *error);
                    task.progress
                        .call(move |progress| match result {
                            Ok((path, bytes)) => progress.complete(path, bytes),
                            Err(error) => progress.state = DownloadState::Failed(error),
                        })
                        .await;
                    task.completed.send_replace(Some(completed));
                    downloads
                        .registry
                        .call({
                            let task = task.clone();
                            move |registry| {
                                if registry
                                    .active
                                    .get(&original.partial.path)
                                    .is_some_and(|(active, _)| active.id == task.id)
                                {
                                    registry.active.remove(&original.partial.path);
                                }
                            }
                        })
                        .await;
                    task.registered.send_if_modified(|current| {
                        if current.is_some() {
                            return false;
                        }
                        *current = Some(registered);
                        true
                    });
                });
                Ok(task)
            })
            .await?;
        let mut registration = task.registered.subscribe();
        let result = registration
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Error::Unavailable)?;
        Ok((task.id, result.expect("下载已登记")?))
    }

    pub(super) fn schedule_cache_cleanup(
        self: &Arc<Self>,
        root: PathBuf,
        team: String,
        retention: std::time::Duration,
    ) {
        let downloads = self.clone();
        self.registry.post(move |registry| {
            let work = registry.teams.entry(team.clone()).or_default().clone();
            let cancellation = work.generation.subscribe();
            tokio::spawn(async move {
                let partial_cleanup = work
                    .partial_cleanup
                    .acquire_owned()
                    .await
                    .expect("半成品维护许可不能关闭");
                if cancellation.has_changed().unwrap_or(true) {
                    return;
                }
                let current_team = team.clone();
                let active = downloads
                    .registry
                    .call(move |registry| {
                        registry
                            .active
                            .iter()
                            .filter(|(_, (task, _))| task.team == current_team)
                            .map(|(path, _)| path.clone())
                            .collect()
                    })
                    .await;
                #[cfg(test)]
                let test_pause = downloads.pause_cleanup.lock().unwrap().take();
                let _ = tokio::task::spawn_blocking(move || {
                    DiskCache::clean_team_until(
                        &root,
                        &team,
                        retention,
                        &active,
                        &work.writes,
                        || cancellation.has_changed().unwrap_or(true),
                        || {
                            drop(partial_cleanup);
                            #[cfg(test)]
                            if let Some((entered, release)) = test_pause {
                                let _ = entered.send(());
                                let _ = release.blocking_recv();
                            }
                        },
                    );
                })
                .await;
            });
        });
    }

    async fn transfer(
        &self,
        service: &SlackService,
        task: &Task,
        original: &Original,
    ) -> Result<(PathBuf, u64), Error> {
        let result = original.transfer(service, task).await;
        if result.is_err() {
            let partial = original.partial.clone();
            let session = task.session;
            service
                .state
                .blocking(move |state| {
                    if state.account(session).is_ok() && partial.read().is_none() {
                        partial.discard();
                    }
                })
                .await;
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
        let session = task.session;
        let token = task.wait(service.session_token(session)).await?;
        let web = task.wait(service.web_session(task.session)).await?;
        let mut resume = self.partial.read().filter(|(offset, metadata)| {
            metadata.source == source
                && strong_etag(&metadata.etag)
                && metadata.total.is_none_or(|total| *offset < total)
        });
        task.progress
            .call(|progress| progress.state = DownloadState::Downloading)
            .await;
        loop {
            let response = task
                .wait(
                    service.client.transfer_original(
                        url,
                        &token,
                        web.as_ref(),
                        resume
                            .as_ref()
                            .map(|(offset, data)| (*offset, data.etag.as_str())),
                    ),
                )
                .await?;
            let partial = self.partial.clone();
            let file_size = self.item.file_id.as_ref().and(self.item.size);
            let source = source.clone();
            let cancellation = task.cancellation.clone();
            let (mut response, setup) = service
                .state
                .blocking(move |state| {
                    state.account(session)?;
                    if cancellation.has_changed().unwrap_or(true) {
                        return Err(Error::DownloadCancelled);
                    }
                    let mut offset = 0;
                    let mut limit = response.content_length();
                    let mut total = limit;
                    let metadata;
                    let mime;
                    let file = match response.status() {
                        StatusCode::OK if identity_encoding(&response) => {
                            total = total.or_else(|| file_size);
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
                            partial.reset()
                        }
                        StatusCode::PARTIAL_CONTENT => {
                            let segment = resume.as_ref().and_then(|(offset, data)| {
                                response_range(&response, *offset, data)
                            });
                            let Some((end, length)) = segment else {
                                if resume.take().is_some() {
                                    partial.discard();
                                    return Ok((response, None));
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
                            partial.append()
                        }
                        StatusCode::RANGE_NOT_SATISFIABLE if resume.take().is_some() => {
                            partial.discard();
                            return Ok((response, None));
                        }
                        status if status.is_server_error() => return Err(Error::Unavailable),
                        _ => return Err(Error::Attachment),
                    }
                    .map_err(|_| Error::Unavailable)?;
                    if let Some(metadata) = &metadata {
                        partial.save(metadata).map_err(|_| Error::Unavailable)?;
                    }
                    Ok((response, Some((file, offset, limit, total, metadata, mime))))
                })
                .await?;
            let Some((file, mut offset, limit, total, metadata, mime)) = setup else {
                resume = None;
                continue;
            };
            let mut file = tokio::fs::File::from_std(file);
            task.progress
                .call(move |progress| {
                    progress.bytes = offset;
                    progress.total = total;
                })
                .await;
            while let Some(chunk) = task
                .wait(async { response.chunk().await.map_err(|_| Error::Unavailable) })
                .await?
            {
                if changes.has_changed().unwrap_or(true) {
                    changes.borrow_and_update();
                    service.check_session(session).await?;
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
                task.progress
                    .call(move |progress| progress.bytes = offset)
                    .await;
            }
            drop(file);
            if limit.is_some_and(|limit| offset != limit) {
                return Err(Error::Unavailable);
            }
            if total.is_some_and(|total| offset < total) {
                resume = metadata.map(|metadata| (offset, metadata));
                continue;
            }
            let path = self
                .cache
                .attachment_destination(&self.item, false, &mime)
                .ok_or(Error::Unavailable)?;
            let partial = self.partial.clone();
            let cache = self.cache.clone();
            let cancellation = task.cancellation.clone();
            return service
                .state
                .blocking(move |state| {
                    state.account(session)?;
                    if cancellation.has_changed().unwrap_or(true) {
                        return Err(Error::DownloadCancelled);
                    }
                    cache
                        .commit_original(&partial, path)
                        .map(|path| (path, offset))
                        .map_err(|_| Error::Unavailable)
                })
                .await;
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
