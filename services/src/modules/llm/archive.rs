use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::body::{Body, Bytes};
use chrono::Utc;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use uuid::Uuid;

use super::{
    config::{MODULE, MONITORING},
    history::request_snapshot,
    instructions::{Capture, Format},
    protocol::{Phase, Protocol, RequestStatus},
    transcript::Input,
    usage::UsageReader,
};
use crate::{http::error::API_VERSION, modules::config::service::ConfigService, process::Process};

const REQUEST: u8 = 1;
const RESPONSE: u8 = 2;
const META: u8 = 3;
const HISTORY_LIMIT: usize = 10;

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Filter {
    protocols: Vec<Protocol>,
    clients: Vec<Uuid>,
    upstreams: Vec<String>,
}

impl Filter {
    fn normalize(&mut self) {
        self.protocols.sort_unstable();
        self.protocols.dedup();
        self.clients.sort_unstable();
        self.clients.dedup();
        self.upstreams.retain(|upstream| !upstream.is_empty());
        self.upstreams.sort_unstable();
        self.upstreams.dedup();
    }

    fn matches(&self, request: &RequestStatus) -> bool {
        (self.protocols.is_empty() || self.protocols.contains(&request.protocol))
            && (self.clients.is_empty() || self.clients.contains(&request.client_id))
            && (self.upstreams.is_empty() || self.upstreams.contains(&request.upstream))
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Settings {
    enabled: bool,
    retention: usize,
    filter: Filter,
    history: Vec<Filter>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            retention: 500,
            filter: Filter::default(),
            history: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsPatch {
    enabled: Option<bool>,
    retention: Option<usize>,
    filter: Option<Filter>,
}

#[derive(Debug)]
pub(crate) struct Archive {
    directory: PathBuf,
    temporary: bool,
    active: Mutex<BTreeSet<Uuid>>,
    changes: Process<()>,
}

impl Archive {
    pub(crate) fn new(directory: Option<PathBuf>) -> Arc<Self> {
        let temporary = directory.is_none();
        Arc::new(Self {
            directory: directory.unwrap_or_else(|| {
                std::env::temp_dir().join(format!("loki-llm-requests-{}", Uuid::new_v4()))
            }),
            temporary,
            active: Mutex::new(BTreeSet::new()),
            changes: Process::start(()),
        })
    }

    pub(super) async fn settings(config: &ConfigService) -> Result<Settings, String> {
        let value = config
            .value(MODULE, MONITORING)
            .await
            .map_err(|err| err.to_string())?;
        let mut settings: Settings =
            serde_json::from_value(value.value).map_err(|err| err.to_string())?;
        settings.filter.normalize();
        let mut history = Vec::new();
        for mut filter in settings.history {
            filter.normalize();
            if !history.contains(&filter) {
                history.push(filter);
                if history.len() == HISTORY_LIMIT {
                    break;
                }
            }
        }
        settings.history = history;
        Ok(settings)
    }

    pub(super) async fn change(
        &self,
        config: Arc<ConfigService>,
        patch: SettingsPatch,
    ) -> Result<Settings, String> {
        self.changes
            .run(async move {
                let mut settings = Self::settings(&config).await?;
                let selected = patch.filter.is_some() || patch.enabled == Some(true);
                if let Some(filter) = patch.filter {
                    settings.filter = filter;
                    settings.filter.normalize();
                }
                if let Some(enabled) = patch.enabled {
                    settings.enabled = enabled;
                }
                if let Some(retention) = patch.retention {
                    settings.retention = retention;
                }
                if settings.enabled && selected {
                    settings.history.retain(|filter| filter != &settings.filter);
                    settings.history.insert(0, settings.filter.clone());
                    settings.history.truncate(HISTORY_LIMIT);
                }
                config
                    .replace(MODULE, MONITORING, json!(settings))
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(settings)
            })
            .await
    }

    pub(super) fn schedule(self: &Arc<Self>, config: Arc<ConfigService>) {
        let archive = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(archive) = archive.upgrade() else {
                    break;
                };
                match Self::settings(&config).await {
                    Ok(settings) => {
                        if let Err(err) = archive.cleanup(settings.retention).await {
                            log::warn!("清理 LLM 请求缓存失败：{err}");
                        }
                    }
                    Err(err) => log::warn!("无法读取 LLM 监控配置，跳过缓存清理：{err}"),
                }
            }
        });
    }

    pub(super) async fn begin(
        self: &Arc<Self>,
        settings: &Settings,
        request: &RequestStatus,
        format: Format,
        identity: bool,
    ) -> io::Result<Option<Writer>> {
        if !settings.enabled || !settings.filter.matches(request) {
            return Ok(None);
        }
        let path = self.path(request.id);
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)
        })
        .await
        .map_err(io::Error::other)??;
        self.active
            .lock()
            .expect("请求缓存锁应可用")
            .insert(request.id);
        let mut registration = Registration {
            id: request.id,
            archive: self.clone(),
            pending_path: Some(path.with_extension("pending")),
            valid: false,
        };
        let pending = registration
            .pending_path
            .as_deref()
            .expect("缓存临时路径应存在");
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pending)
            .await?;
        let start = Start {
            item: request_snapshot(request, None, None),
            format,
            identity,
        };
        write_frame(&mut file, META, &serde_json::to_vec(&start)?).await?;
        tokio::fs::rename(pending, path).await?;
        registration.pending_path = None;
        registration.valid = true;
        Ok(Some(Writer(Arc::new(tokio::sync::Mutex::new((
            registration,
            file,
        ))))))
    }

    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.log"))
    }

    pub(super) async fn annotate(self: &Arc<Self>, mut list: Value) -> Value {
        let archive = self.clone();
        tokio::task::spawn_blocking(move || {
            for key in ["active", "recent"] {
                for row in list[key].as_array_mut().into_iter().flatten() {
                    let available = row["id"]
                        .as_str()
                        .and_then(|id| Uuid::parse_str(id).ok())
                        .is_some_and(|id| archive.path(id).is_file());
                    row["has_detail"] = json!(available);
                }
            }
            list
        })
        .await
        .expect("请求缓存状态任务应正常结束")
    }

    pub(super) async fn detail(self: &Arc<Self>, id: Uuid) -> io::Result<Value> {
        let archive = self.clone();
        tokio::task::spawn_blocking(move || {
            let live = archive
                .active
                .lock()
                .expect("请求缓存锁应可用")
                .contains(&id);
            load_detail(&archive.path(id), live)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(super) async fn raw(&self, id: Uuid, response: bool) -> io::Result<Body> {
        let file = tokio::fs::File::open(self.path(id)).await?;
        let end = file.metadata().await?.len();
        let wanted = if response { RESPONSE } else { REQUEST };
        let frames = stream::try_unfold(
            (file, 0_u64, 0_u64),
            move |(mut file, mut offset, mut pending)| async move {
                while pending == 0 {
                    if end.saturating_sub(offset) < 9 {
                        return Ok::<_, io::Error>(None);
                    }
                    let mut header = [0; 9];
                    file.read_exact(&mut header).await?;
                    offset += 9;
                    let size = u64::from_be_bytes(
                        header[1..].try_into().expect("缓存长度字段应为 8 字节"),
                    );
                    if size > end - offset {
                        return Ok(None);
                    }
                    if header[0] == wanted {
                        pending = size;
                    } else {
                        offset += size;
                        file.seek(SeekFrom::Start(offset)).await?;
                    }
                }
                let mut bytes = vec![0; pending.min(16384) as usize];
                file.read_exact(&mut bytes).await?;
                let size = bytes.len() as u64;
                Ok(Some((
                    Bytes::from(bytes),
                    (file, offset + size, pending - size),
                )))
            },
        );
        Ok(Body::from_stream(frames))
    }

    pub(super) async fn cleanup(self: &Arc<Self>, retention: usize) -> io::Result<()> {
        let archive = self.clone();
        tokio::task::spawn_blocking(move || {
            let entries = match fs::read_dir(&archive.directory) {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err),
            };
            let mut entries_by_time = Vec::new();
            for entry in entries {
                let path = entry?.path();
                let extension = path.extension().and_then(|ext| ext.to_str());
                if !matches!(extension, Some("log" | "pending")) {
                    continue;
                }
                let Some(id) = path
                    .file_stem()
                    .and_then(|id| id.to_str())
                    .and_then(|id| Uuid::parse_str(id).ok())
                else {
                    continue;
                };
                let active = archive
                    .active
                    .lock()
                    .expect("请求缓存锁应可用")
                    .contains(&id);
                if extension == Some("pending") {
                    if !active {
                        match fs::remove_file(path) {
                            Ok(()) => {}
                            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                            Err(err) => return Err(err),
                        }
                    }
                    continue;
                }
                match open_detail(&path) {
                    Ok((start, _, _)) => {
                        let started = start.item["started_at"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        entries_by_time.push((started, id, path, active));
                    }
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        if !active {
                            fs::remove_file(path)?;
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
            entries_by_time.sort_unstable_by(|a, b| b.cmp(a));
            for (_, _, path, active) in entries_by_time.into_iter().skip(retention) {
                if !active {
                    fs::remove_file(path)?;
                }
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        if self.temporary {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

struct Registration {
    id: Uuid,
    archive: Arc<Archive>,
    pending_path: Option<PathBuf>,
    valid: bool,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending_path {
            let _ = fs::remove_file(pending);
        }
        self.archive
            .active
            .lock()
            .expect("请求缓存锁应可用")
            .remove(&self.id);
    }
}

#[derive(Clone)]
pub(super) struct Writer(Arc<tokio::sync::Mutex<(Registration, tokio::fs::File)>>);

#[derive(Deserialize, Serialize)]
struct Start {
    item: Value,
    format: Format,
    identity: bool,
}

#[derive(Deserialize, Serialize)]
pub(super) enum Update {
    Headers {
        status: u16,
        stream: bool,
        identity: bool,
    },
    FirstByte(u64),
    Finished {
        phase: Phase,
        duration: u64,
        finished: String,
        body_complete: bool,
    },
}

impl Writer {
    pub(super) async fn request(&self, bytes: &[u8]) {
        self.write(REQUEST, bytes).await;
    }
    pub(super) async fn response(&self, bytes: &[u8]) {
        self.write(RESPONSE, bytes).await;
    }

    pub(super) async fn update(&self, event: Update) {
        if let Ok(bytes) = serde_json::to_vec(&event) {
            self.write(META, &bytes).await;
        }
    }

    async fn write(&self, kind: u8, bytes: &[u8]) {
        let mut writer = self.0.lock().await;
        let (registration, file) = &mut *writer;
        if std::mem::replace(&mut registration.valid, false) {
            match write_frame(file, kind, bytes).await {
                Ok(()) => registration.valid = true,
                Err(err) => {
                    log::warn!("写入 LLM 请求缓存失败：{err}");
                }
            }
        }
    }
}

async fn write_frame(file: &mut tokio::fs::File, kind: u8, bytes: &[u8]) -> io::Result<()> {
    let mut header = [kind; 9];
    header[1..].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
    file.write_all(&header).await?;
    file.write_all(bytes).await?;
    file.flush().await
}

fn open_detail(path: &Path) -> io::Result<(Start, BufReader<File>, u64)> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut file = BufReader::new(file);
    let Some((META, head_size)) = frame_header(&mut file, length)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "请求缓存缺少首条记录",
        ));
    };
    let start = serde_json::from_reader((&mut file).take(head_size))?;
    Ok((start, file, length))
}

fn frame_header(file: &mut BufReader<File>, end: u64) -> io::Result<Option<(u8, u64)>> {
    let mut header = [0; 9];
    let remaining = end.saturating_sub(file.stream_position()?);
    if remaining < header.len() as u64 {
        return Ok(None);
    }
    file.read_exact(&mut header)?;
    let size = u64::from_be_bytes(header[1..].try_into().expect("缓存长度字段应为 8 字节"));
    Ok((size <= remaining - header.len() as u64).then_some((header[0], size)))
}

fn load_detail(path: &Path, live: bool) -> io::Result<Value> {
    let (start, mut file, end) = open_detail(path)?;
    let mut item = start.item;
    let protocol = serde_json::from_value(item["protocol"].clone())?;
    let mut request = Vec::new();
    let mut output = None;
    let mut body_complete = false;
    while let Some((kind, size)) = frame_header(&mut file, end)? {
        let mut frame = (&mut file).take(size);
        match kind {
            REQUEST if start.identity => {
                frame.read_to_end(&mut request)?;
            }
            RESPONSE if output.is_some() => {
                let reader: &mut UsageReader = output.as_mut().expect("响应解析器应存在");
                let mut buffer = [0; 16384];
                loop {
                    let count = frame.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    reader.push(&buffer[..count]);
                }
            }
            META => match serde_json::from_reader(frame)? {
                Update::Headers {
                    status,
                    stream,
                    identity,
                } => {
                    item["upstream_status"] = json!(status);
                    if identity {
                        let mut reader = UsageReader::new(protocol, stream);
                        reader.capture_output(start.format);
                        output = Some(reader);
                    }
                }
                Update::FirstByte(ms) => {
                    item["first_byte_ms"] = json!(ms);
                    item["phase"] = json!("responding");
                }
                Update::Finished {
                    phase,
                    duration,
                    finished,
                    body_complete: complete,
                } => {
                    item["phase"] = json!(phase);
                    item["duration_ms"] = json!(duration);
                    item["finished_at"] = json!(finished);
                    body_complete = complete;
                }
            },
            _ => {
                io::copy(&mut frame, &mut io::sink())?;
            }
        }
    }
    if !live && item["finished_at"].is_null() {
        item["phase"] = json!("cancelled");
    }
    let content = serde_json::from_slice::<Value>(&request)
        .ok()
        .filter(Value::is_object);
    let captured = content
        .as_ref()
        .map(|body| Capture::Captured(start.format.read(body)))
        .unwrap_or(Capture::Unavailable);
    item["model"] = content
        .as_ref()
        .map(|body| body["model"].clone())
        .unwrap_or(Value::Null);
    item["instructions"] = captured.view();
    item["request_content"] = json!(content.as_ref().map(|body| Input::read(start.format, body)));
    if let Some(output) = output {
        let (observed, failed) = output.finish(body_complete);
        if failed && item["phase"] == "completed" {
            item["phase"] = json!("failed");
        }
        if let Some(model) = observed.model {
            item["model"] = json!(model);
        }
        if !item["finished_at"].is_null() {
            item["usage"] = json!(observed.usage);
        }
        item["response_content"] = observed
            .output
            .map(|output| output.view())
            .unwrap_or(Value::Null);
    } else {
        item["response_content"] = Value::Null;
    }
    item["has_detail"] = json!(true);
    Ok(json!({"api_version":API_VERSION,"item":item}))
}

pub(super) fn finished(phase: Phase, duration: u64, body_complete: bool) -> Update {
    Update::Finished {
        phase,
        duration,
        finished: Utc::now().to_rfc3339(),
        body_complete,
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    #[tokio::test]
    async fn cleanup_orders_by_start_time_and_preserves_active_files() {
        let archive = Archive::new(None);
        let mut filter = Settings::default();
        filter.filter.protocols.push(Protocol::Codex);
        let mut request = RequestStatus {
            id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            session_id: None,
            upstream: "work".into(),
            protocol: Protocol::Claude,
            model: None,
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            phase: Phase::Waiting,
            first_byte_ms: None,
            duration_ms: None,
            upstream_status: None,
        };
        assert!(
            archive
                .begin(&filter, &request, Format::Messages, true)
                .await
                .unwrap()
                .is_none()
        );
        request.protocol = Protocol::Codex;
        let latest = request.id;
        let writer = archive
            .begin(&filter, &request, Format::Responses, true)
            .await
            .unwrap()
            .unwrap();
        writer.request(br#"{"instructions":"saved"}"#).await;
        writer.update(finished(Phase::Completed, 1, true)).await;
        drop(writer);

        // 文件以请求时间排序，较晚创建的文件也可能属于更早开始的请求。
        request.id = Uuid::new_v4();
        request.started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let earliest = request.id;
        let writer = archive
            .begin(&filter, &request, Format::Responses, true)
            .await
            .unwrap()
            .unwrap();
        archive.cleanup(1).await.unwrap();
        assert!(archive.path(earliest).is_file());
        assert!(archive.path(latest).is_file());
        drop(writer);
        archive.cleanup(1).await.unwrap();
        assert!(!archive.path(earliest).exists());

        let restarted = Archive::new(Some(archive.directory.clone()));
        let detail = restarted.detail(latest).await.unwrap();
        assert_eq!(detail["item"]["instructions"]["blocks"][0]["text"], "saved");
        assert_eq!(detail["item"]["phase"], "completed");
    }
}
