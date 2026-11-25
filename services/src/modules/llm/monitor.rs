use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::protocol::{
    Client, Phase, Protocol, RequestStatus, SessionUsageRow, Usage, UsageRow, UsageTotal,
};
use crate::{http::error::API_VERSION, process::Process};

const RECENT_REQUESTS: usize = 100;
type Totals = BTreeMap<Uuid, BTreeMap<String, UpstreamUsage>>;

#[derive(Default, Deserialize, Serialize)]
struct UpstreamUsage {
    #[serde(flatten)]
    total: UsageTotal,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, UsageTotal>,
}

#[derive(Debug)]
pub(crate) struct Monitor(Process<State>);

struct State {
    active: BTreeMap<Uuid, RequestStatus>,
    recent: VecDeque<RequestStatus>,
    totals: Totals,
    path: Option<PathBuf>,
    storage_error: bool,
}

#[derive(Serialize)]
pub(super) struct Requests {
    api_version: &'static str,
    active: Vec<RequestStatus>,
    recent: Vec<RequestStatus>,
}

#[derive(Serialize)]
pub(super) struct UsageReport<Row> {
    api_version: &'static str,
    storage_error: bool,
    usage: Vec<Row>,
}

impl Monitor {
    pub(crate) fn memory() -> Self {
        Self::new(None, Totals::new())
    }

    pub(crate) fn load(path: PathBuf) -> io::Result<Self> {
        let totals = match fs::File::open(&path) {
            Ok(file) => serde_json::from_reader(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Totals::new(),
            Err(error) => return Err(error),
        };
        Ok(Self::new(Some(path), totals))
    }

    fn new(path: Option<PathBuf>, totals: Totals) -> Self {
        Self(Process::start(State {
            active: BTreeMap::new(),
            recent: VecDeque::new(),
            totals,
            path,
            storage_error: false,
        }))
    }

    pub(super) fn start(
        &self,
        client: Client,
        protocol: Protocol,
        headers: &HeaderMap,
        endpoint: String,
        track_usage: bool,
    ) -> RequestGuard {
        let id = Uuid::new_v4();
        let request = RequestStatus {
            id,
            client_id: client.id,
            session_id: session_id(headers, protocol),
            upstream: client.binding.upstream,
            protocol,
            endpoint,
            started_at: SystemTime::now(),
            phase: Phase::Waiting,
            first_byte_ms: None,
            duration_ms: None,
            upstream_status: None,
            usage: None,
        };
        self.0.post(move |state| {
            state.active.insert(id, request);
        });
        RequestGuard {
            monitor: self.0.clone(),
            id: Some(id),
            started: Instant::now(),
            track_usage,
        }
    }

    pub(super) async fn requests(&self) -> Requests {
        self.0
            .call(|state| {
                let mut active: Vec<_> = state.active.values().cloned().collect();
                active.sort_by_key(|request| request.started_at);
                Requests {
                    api_version: API_VERSION,
                    active,
                    recent: state.recent.iter().cloned().collect(),
                }
            })
            .await
    }

    pub(super) async fn usage(&self) -> UsageReport<UsageRow> {
        self.0
            .call(|state| UsageReport {
                api_version: API_VERSION,
                storage_error: state.storage_error,
                usage: state
                    .totals
                    .iter()
                    .flat_map(|(&client_id, upstreams)| {
                        upstreams.iter().map(move |(upstream, total)| UsageRow {
                            client_id,
                            upstream: upstream.clone(),
                            total: total.total.clone(),
                        })
                    })
                    .collect(),
            })
            .await
    }

    pub(super) async fn session_usage(&self) -> UsageReport<SessionUsageRow> {
        self.0
            .call(|state| UsageReport {
                api_version: API_VERSION,
                storage_error: state.storage_error,
                usage: state
                    .totals
                    .iter()
                    .flat_map(|(&client_id, upstreams)| {
                        upstreams.iter().flat_map(move |(upstream, totals)| {
                            totals
                                .sessions
                                .iter()
                                .map(move |(session_id, total)| SessionUsageRow {
                                    session_id: session_id.clone(),
                                    usage: UsageRow {
                                        client_id,
                                        upstream: upstream.clone(),
                                        total: total.clone(),
                                    },
                                })
                        })
                    })
                    .collect(),
            })
            .await
    }
}

fn session_id(headers: &HeaderMap, protocol: Protocol) -> Option<String> {
    let candidates: &[&str] = match protocol {
        Protocol::Codex => &["x-loki-session-id", "thread-id", "session-id", "session_id"],
        Protocol::Claude => &["x-loki-session-id"],
    };
    candidates.iter().find_map(|&name| {
        let values = headers.get_all(name);
        let mut values = values.iter();
        let value = values.next()?.to_str().ok()?;
        (values.next().is_none()
            && !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b','))
        .then(|| value.to_owned())
    })
}

pub(super) struct RequestGuard {
    monitor: Process<State>,
    id: Option<Uuid>,
    started: Instant,
    track_usage: bool,
}

impl RequestGuard {
    pub(super) fn upstream_status(&self, status: u16) {
        let id = self.id.expect("请求尚未结束");
        self.monitor.post(move |state| {
            state
                .active
                .get_mut(&id)
                .expect("请求已经登记")
                .upstream_status = Some(status);
        });
    }

    pub(super) fn first_byte(&self, stream: bool) {
        let id = self.id.expect("请求尚未结束");
        let elapsed = millis(self.started);
        self.monitor.post(move |state| {
            let request = state.active.get_mut(&id).expect("请求已经登记");
            request.first_byte_ms = Some(elapsed);
            if stream {
                request.phase = Phase::Streaming;
            }
        });
    }

    pub(super) fn finish(&mut self, phase: Phase, usage: Option<Usage>) {
        let Some(id) = self.id.take() else { return };
        let usage = self.track_usage.then(|| usage.unwrap_or_default());
        let duration_ms = millis(self.started);
        self.monitor
            .post_async(move |state| finish(state, id, phase, usage, duration_ms));
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.finish(Phase::Cancelled, None);
    }
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn finish(
    mut state: State,
    id: Uuid,
    phase: Phase,
    usage: Option<Usage>,
    duration_ms: u64,
) -> State {
    let mut request = state.active.remove(&id).expect("请求已经登记");
    request.phase = phase;
    request.duration_ms = Some(duration_ms);
    request.usage = usage;
    if let Some(usage) = usage {
        let totals = state
            .totals
            .entry(request.client_id)
            .or_default()
            .entry(request.upstream.clone())
            .or_default();
        totals.total.record(usage);
        if let Some(session_id) = &request.session_id {
            totals
                .sessions
                .entry(session_id.clone())
                .or_default()
                .record(usage);
        }
    }
    state.recent.push_front(request);
    state.recent.truncate(RECENT_REQUESTS);
    if usage.is_some()
        && let Some(path) = state.path.clone()
    {
        state = tokio::task::spawn_blocking(move || {
            state.storage_error = persist(&path, &state.totals).is_err();
            state
        })
        .await
        .expect("用量存储任务应正常结束");
    }
    state
}

fn persist(path: &Path, totals: &Totals) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, totals)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
