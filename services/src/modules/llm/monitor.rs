use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use axum::{body::Bytes, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::protocol::{
    Client, Phase, Protocol, RequestStatus, SessionUsageRow, UsageRow, UsageTotal,
};
use super::{
    history::{
        self, Dimension, Error, Event, Filters, History, OptionsQuery, RequestsQuery, SummaryQuery,
    },
    usage::{ObservedUsage, UsageReader},
};
use crate::{http::error::API_VERSION, process::Process};

type Totals = BTreeMap<Uuid, BTreeMap<String, UpstreamUsage>>;

#[derive(Default, Deserialize, Serialize)]
struct UpstreamUsage {
    #[serde(flatten)]
    total: UsageTotal,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sessions: BTreeMap<String, UsageTotal>,
}

#[derive(Debug)]
pub(crate) struct Monitor(Process<State>, Arc<Semaphore>);

const OBSERVATION_QUEUE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODEL_REQUEST_BYTES: usize = 1024 * 1024;

struct State {
    active: BTreeMap<Uuid, RequestStatus>,
    observers: BTreeMap<Uuid, UsageReader>,
    request_bodies: BTreeMap<Uuid, Vec<(Bytes, OwnedSemaphorePermit)>>,
    totals: Totals,
    history: History,
    pending: Vec<Event>,
    storage_error: bool,
}

#[derive(Serialize)]
pub(super) struct UsageReport<Row> {
    api_version: &'static str,
    storage_error: bool,
    usage: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<Option<String>>,
}

impl Monitor {
    pub(crate) fn memory() -> Self {
        Self::open(None).expect("应能打开内存用量数据库")
    }

    pub(crate) fn load(path: PathBuf) -> io::Result<Self> {
        Self::open(Some(path))
    }

    fn open(path: Option<PathBuf>) -> io::Result<Self> {
        let legacy = match &path {
            Some(path) => match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => "{}".into(),
                Err(error) => return Err(error),
            },
            None => "{}".into(),
        };
        let path = path.map(|path| path.with_file_name("llm-usage.sqlite3"));
        let (mut history, legacy) = History::open(path.as_deref(), &legacy)?;
        let mut totals: Totals = serde_json::from_str(&legacy)?;
        for (client, upstream, session, usage) in history.totals().map_err(io::Error::other)? {
            let entry = totals
                .entry(client)
                .or_default()
                .entry(upstream)
                .or_default();
            entry.total.add(&usage);
            if let Some(session) = session {
                entry.sessions.entry(session).or_default().add(&usage);
            }
        }
        history.merge().map_err(io::Error::other)?;
        let task = Process::start(State {
            active: BTreeMap::new(),
            observers: BTreeMap::new(),
            request_bodies: BTreeMap::new(),
            totals,
            history,
            pending: Vec::new(),
            storage_error: false,
        });
        let weak = task.downgrade();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            let mut next_merge = Instant::now() + Duration::from_secs(60);
            loop {
                tick.tick().await;
                let Some(task) = weak.upgrade() else { break };
                let merge = Instant::now() >= next_merge;
                task.blocking(move |state| {
                    state.flush();
                    if merge && let Err(error) = state.history.merge() {
                        log::warn!("合并 LLM 用量日报失败：{error}");
                    }
                })
                .await;
                if merge {
                    next_merge = Instant::now() + Duration::from_secs(60);
                }
            }
        });
        Ok(Self(
            task,
            Arc::new(Semaphore::new(OBSERVATION_QUEUE_BYTES)),
        ))
    }

    pub(super) async fn history(&self, query: RequestsQuery) -> Result<Value, Error> {
        self.0
            .blocking(move |state| {
                state.flush();
                state.history.requests(query, state.storage_error)
            })
            .await
    }

    pub(super) async fn requests(&self) -> Result<Value, Error> {
        const LIMIT: usize = 100;
        self.0
            .blocking(|state| {
                let mut active: Vec<_> = state.active.values().collect();
                active.sort_by_key(|request| (request.started_at, request.id));
                let active: Vec<_> = active
                    .into_iter()
                    .map(|request| history::request_snapshot(request, None, None))
                    .collect();

                let mut recent: Vec<_> = state.pending.iter().rev().map(Event::snapshot).collect();
                recent.extend(state.history.recent(LIMIT)?);
                recent.sort_by_key(|(finished, _)| std::cmp::Reverse(*finished));
                let recent: Vec<_> = recent
                    .into_iter()
                    .take(LIMIT)
                    .map(|(_, request)| request)
                    .collect();
                Ok(serde_json::json!({
                    "api_version": API_VERSION,
                    "active": active,
                    "recent": recent,
                    "storage_error": state.storage_error,
                }))
            })
            .await
    }

    pub(super) async fn detail(&self, id: Uuid) -> Result<Value, Error> {
        self.0
            .blocking(move |state| {
                state.flush();
                state.history.request(id, state.storage_error)
            })
            .await
    }

    pub(super) async fn summary(&self, query: SummaryQuery) -> Result<Value, Error> {
        self.0
            .blocking(move |state| {
                state.flush();
                state.history.summary(query, state.storage_error)
            })
            .await
    }

    pub(super) async fn options(&self, query: OptionsQuery) -> Result<Value, Error> {
        self.0
            .blocking(move |state| {
                state.flush();
                if query.source.as_deref() != Some("sessions") {
                    return state.history.options(query);
                }
                let column = query.session_column()?;
                let limit = history::limit(query.limit.as_deref(), 30)?;
                let mut values = BTreeSet::new();
                for (&client, upstreams) in &state.totals {
                    for (upstream, totals) in upstreams {
                        if totals
                            .sessions
                            .keys()
                            .any(|session| query.filters.matches_session(client, upstream, session))
                        {
                            let value = match column {
                                Dimension::ClientId => client.to_string(),
                                _ => upstream.clone(),
                            };
                            if query
                                .q
                                .as_deref()
                                .is_none_or(|prefix| value.starts_with(prefix))
                            {
                                values.insert(value);
                            }
                        }
                    }
                }
                Ok(history::option_report(
                    values.into_iter().take(limit + 1).collect(),
                    limit,
                ))
            })
            .await
    }

    pub(super) fn start(
        &self,
        client: Client,
        protocol: Protocol,
        headers: &HeaderMap,
        track_usage: bool,
    ) -> RequestGuard {
        let id = Uuid::new_v4();
        let request = RequestStatus {
            id,
            client_id: client.id,
            session_id: session_id(headers, protocol),
            upstream: client.binding.upstream,
            protocol,
            model: None,
            started_at: SystemTime::now(),
            phase: Phase::Waiting,
            first_byte_ms: None,
            duration_ms: None,
            upstream_status: None,
        };
        self.0.post(move |state| {
            state.active.insert(id, request);
        });
        RequestGuard {
            monitor: self.0.clone(),
            id: Some(id),
            started: Instant::now(),
            budget: self.1.clone(),
            track_usage,
            observe_usage: false,
            observation_lost: false,
        }
    }

    pub(crate) async fn flush(&self) -> bool {
        self.0
            .blocking(|state| {
                state.flush();
                !state.storage_error
            })
            .await
    }

    pub(super) async fn usage(&self) -> UsageReport<UsageRow> {
        self.0
            .blocking(|state| {
                state.flush();
                UsageReport {
                    api_version: API_VERSION,
                    storage_error: state.storage_error,
                    next_cursor: None,
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
                }
            })
            .await
    }

    pub(super) async fn session_usage(
        &self,
        query: SessionQuery,
    ) -> Result<UsageReport<SessionUsageRow>, Error> {
        query.filters.validate()?;
        let page = query.limit.is_some() || query.cursor.is_some();
        let limit = history::limit(query.limit.as_deref(), 50)?;
        let filter_key = history::encode_cursor(&query.filters);
        let cursor = query
            .cursor
            .as_deref()
            .map(history::decode_cursor::<SessionCursor>)
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.filter != filter_key)
        {
            return Err(Error::Invalid);
        }
        self.0
            .blocking(move |state| {
                state.flush();
                let mut rows = Vec::new();
                for (&client_id, upstreams) in &state.totals {
                    for (upstream, totals) in upstreams {
                        for (session_id, total) in &totals.sessions {
                            if query
                                .filters
                                .matches_session(client_id, upstream, session_id)
                            {
                                rows.push(SessionUsageRow {
                                    session_id: session_id.clone(),
                                    usage: UsageRow {
                                        client_id,
                                        upstream: upstream.clone(),
                                        total: total.clone(),
                                    },
                                });
                            }
                        }
                    }
                }
                let next_cursor = page.then(|| {
                    rows.sort_by(|a, b| {
                        a.usage
                            .client_id
                            .cmp(&b.usage.client_id)
                            .then_with(|| a.session_id.cmp(&b.session_id))
                            .then_with(|| a.usage.upstream.cmp(&b.usage.upstream))
                    });
                    if let Some(cursor) = cursor {
                        rows.retain(|row| {
                            (
                                row.usage.client_id,
                                row.session_id.as_str(),
                                row.usage.upstream.as_str(),
                            ) > (
                                cursor.client,
                                cursor.session.as_str(),
                                cursor.upstream.as_str(),
                            )
                        });
                    }
                    if rows.len() <= limit {
                        return None;
                    }
                    rows.truncate(limit);
                    rows.last().map(|row| {
                        history::encode_cursor(&SessionCursor {
                            filter: filter_key,
                            client: row.usage.client_id,
                            session: row.session_id.clone(),
                            upstream: row.usage.upstream.clone(),
                        })
                    })
                });
                Ok(UsageReport {
                    api_version: API_VERSION,
                    storage_error: state.storage_error,
                    usage: rows,
                    next_cursor,
                })
            })
            .await
    }
}

#[derive(Default, Deserialize)]
pub(super) struct SessionQuery {
    #[serde(flatten)]
    filters: Filters,
    limit: Option<String>,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct SessionCursor {
    filter: String,
    client: Uuid,
    session: String,
    upstream: String,
}

impl State {
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.storage_error = match self.history.write(&self.pending) {
            Ok(()) => {
                self.pending.clear();
                false
            }
            Err(error) => {
                log::warn!("写入 LLM 用量明细失败：{error}");
                true
            }
        };
    }
}

fn session_id(headers: &HeaderMap, protocol: Protocol) -> Option<String> {
    let candidates: &[&str] = match protocol {
        Protocol::Codex => &["thread-id", "session-id", "session_id"],
        Protocol::Claude => return None,
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
    budget: Arc<Semaphore>,
    track_usage: bool,
    observe_usage: bool,
    observation_lost: bool,
}

impl RequestGuard {
    pub(super) fn request_body_observer(&self) -> RequestBodyObserver {
        RequestBodyObserver {
            monitor: self.monitor.clone(),
            budget: self.budget.clone(),
            id: self.id.expect("请求尚未结束"),
            remaining: MAX_MODEL_REQUEST_BYTES,
        }
    }

    pub(super) fn response_headers(
        &mut self,
        status: u16,
        protocol: Protocol,
        stream: bool,
        identity: bool,
    ) {
        let id = self.id.expect("请求尚未结束");
        self.observe_usage = self.track_usage && identity;
        let observe_usage = self.observe_usage;
        self.monitor.post(move |state| {
            finish_request_body(state, id);
            state
                .active
                .get_mut(&id)
                .expect("请求已经登记")
                .upstream_status = Some(status);
            if observe_usage {
                state
                    .observers
                    .insert(id, UsageReader::new(protocol, stream));
            }
        });
    }

    pub(super) fn body_bytes(&mut self, chunk: &Bytes) {
        if !self.observe_usage || self.observation_lost || chunk.is_empty() {
            return;
        }
        let Ok(cost) = u32::try_from(chunk.len().max(64)) else {
            self.observation_lost = true;
            return;
        };
        let Ok(permit) = self.budget.clone().try_acquire_many_owned(cost) else {
            self.observation_lost = true;
            return;
        };
        let id = self.id.expect("请求尚未结束");
        let chunk = chunk.clone();
        self.monitor.post(move |state| {
            state
                .observers
                .get_mut(&id)
                .expect("响应已登记")
                .push(&chunk);
            drop(permit);
        });
    }

    pub(super) fn first_byte(&self) {
        let id = self.id.expect("请求尚未结束");
        let elapsed = millis(self.started);
        self.monitor.post(move |state| {
            let request = state.active.get_mut(&id).expect("请求已经登记");
            request.first_byte_ms = Some(elapsed);
        });
    }

    pub(super) fn finish(&mut self, phase: Phase, body_complete: bool) {
        let Some(id) = self.id.take() else { return };
        let track_usage = self.track_usage;
        let observation_lost = self.observation_lost;
        let duration_ms = millis(self.started);
        self.monitor.post_async(move |mut state| async move {
            finish_request_body(&mut state, id);
            let reading = state
                .observers
                .remove(&id)
                .map(|reader| reader.finish(body_complete && !observation_lost));
            let phase = if matches!(phase, Phase::Completed)
                && !observation_lost
                && reading.as_ref().is_some_and(|(_, failed)| *failed)
            {
                Phase::Failed
            } else {
                phase
            };
            let observed = track_usage.then(|| {
                let mut observed = reading.map(|(usage, _)| usage).unwrap_or_default();
                if observation_lost {
                    observed.usage.complete = false;
                }
                observed
            });
            finish(state, id, phase, observed, duration_ms).await
        });
    }
}

pub(super) struct RequestBodyObserver {
    monitor: Process<State>,
    budget: Arc<Semaphore>,
    id: Uuid,
    remaining: usize,
}

impl RequestBodyObserver {
    pub(super) fn push(&mut self, chunk: &Bytes) {
        if chunk.is_empty() || self.remaining == 0 {
            return;
        }
        let permit = (chunk.len() <= self.remaining)
            .then(|| {
                self.budget
                    .clone()
                    .try_acquire_many_owned(chunk.len().max(64) as u32)
                    .ok()
            })
            .flatten();
        let id = self.id;
        if let Some(permit) = permit {
            self.remaining -= chunk.len();
            let chunk = chunk.clone();
            self.monitor.post(move |state| {
                if state
                    .active
                    .get(&id)
                    .is_some_and(|request| request.upstream_status.is_none())
                {
                    state
                        .request_bodies
                        .entry(id)
                        .or_default()
                        .push((chunk, permit));
                }
            });
        } else {
            self.remaining = 0;
            self.monitor.post(move |state| {
                state.request_bodies.remove(&id);
            });
        }
    }
}

impl Drop for RequestBodyObserver {
    fn drop(&mut self) {
        let id = self.id;
        self.monitor
            .post(move |state| finish_request_body(state, id));
    }
}

fn finish_request_body(state: &mut State, id: Uuid) {
    #[derive(Deserialize)]
    struct RequestModel {
        model: String,
    }

    if let Some(chunks) = state.request_bodies.remove(&id)
        && let Some(request) = state.active.get_mut(&id)
        && request.model.is_none()
    {
        let mut bytes = Vec::with_capacity(chunks.iter().map(|(bytes, _)| bytes.len()).sum());
        for (chunk, _permit) in &chunks {
            bytes.extend_from_slice(chunk);
        }
        if let Ok(value) = serde_json::from_slice::<RequestModel>(&bytes)
            && !value.model.is_empty()
        {
            request.model = Some(value.model);
        }
    }
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.finish(Phase::Cancelled, false);
    }
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn finish(
    mut state: State,
    id: Uuid,
    phase: Phase,
    observed: Option<ObservedUsage>,
    duration_ms: u64,
) -> State {
    let mut request = state.active.remove(&id).expect("请求已经登记");
    request.phase = phase;
    request.duration_ms = Some(duration_ms);
    if let Some(observed) = observed {
        if observed.model.is_some() {
            request.model = observed.model.clone();
        }
        let usage = observed.usage;
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
        state.pending.push(Event::new(request, observed));
    }
    if state.pending.len() >= 128 {
        state = tokio::task::spawn_blocking(move || {
            state.flush();
            state
        })
        .await
        .expect("用量存储任务应正常结束");
    }
    state
}
