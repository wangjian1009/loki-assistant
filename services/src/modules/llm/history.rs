use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt, path::Path};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Days, FixedOffset, NaiveDate, Utc};
use rusqlite::{
    Connection, OptionalExtension, Row, params, params_from_iter, types::Value as SqlValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    protocol::{Phase, Protocol, RequestStatus, Usage},
    usage::ObservedUsage,
};
use crate::http::error::API_VERSION;

const DIMENSIONS: &str = "report_day, client_id, upstream_id, protocol, model";
const COUNTERS: &[&str] = &[
    "requests",
    "incomplete_requests",
    "input_reported_tokens",
    "input_uncached_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
];
const SUM_EVENTS: &str = "count(*) AS requests, coalesce(sum(incomplete),0) AS incomplete_requests,
    coalesce(sum(input_reported_tokens),0) AS input_reported_tokens,
    coalesce(sum(input_uncached_tokens),0) AS input_uncached_tokens,
    coalesce(sum(cache_read_tokens),0) AS cache_read_tokens,
    coalesce(sum(cache_write_tokens),0) AS cache_write_tokens,
    coalesce(sum(output_tokens),0) AS output_tokens";
const DETAIL: &str = "request_id, client_id, session_id, upstream_id, protocol, model,
    started_at_ms, finished_at_ms, outcome, upstream_status, first_byte_ms, duration_ms,
    input_reported_tokens, input_uncached_tokens, cache_read_tokens, cache_write_tokens,
    output_tokens, incomplete";
const ZONE: &str = "Asia/Shanghai";

#[derive(Debug)]
pub(super) enum Error {
    Invalid,
    NotFound,
    Store,
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        log::warn!("读取 LLM 用量数据库失败：{error}");
        Self::Store
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct Filters {
    pub client_id: Option<Uuid>,
    pub upstream: Option<String>,
    pub protocol: Option<Protocol>,
    pub model: Option<String>,
    pub session_id: Option<String>,
}

impl Filters {
    pub fn validate(&self) -> Result<(), Error> {
        if self.session_id.is_some() && self.client_id.is_none() {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn matches_session(&self, client_id: Uuid, upstream: &str, session_id: &str) -> bool {
        self.client_id.is_none_or(|id| id == client_id)
            && self.upstream.as_deref().is_none_or(|id| id == upstream)
            && self.session_id.as_deref().is_none_or(|id| id == session_id)
    }

    fn apply(&self, conditions: &mut Conditions) {
        for (column, value) in [
            ("client_id", self.client_id.map(|id| id.to_string())),
            ("upstream_id", self.upstream.clone()),
            (
                "protocol",
                self.protocol.map(|p| protocol_name(p).to_owned()),
            ),
            ("model", self.model.clone()),
            ("session_id", self.session_id.clone()),
        ] {
            if let Some(value) = value {
                conditions.bind(&format!("{column} = ?"), value);
            }
        }
    }
}

#[derive(Default)]
struct Conditions {
    parts: Vec<String>,
    values: Vec<SqlValue>,
}

impl Conditions {
    fn bind(&mut self, clause: &str, value: impl Into<SqlValue>) {
        self.parts.push(clause.to_owned());
        self.values.push(value.into());
    }

    fn sql(&self) -> String {
        if self.parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.parts.join(" AND "))
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct RequestsQuery {
    #[serde(flatten)]
    filters: Filters,
    from: Option<String>,
    to: Option<String>,
    outcome: Option<String>,
    incomplete: Option<String>,
    limit: Option<String>,
    #[serde(skip_serializing)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SummaryQuery {
    #[serde(flatten)]
    filters: Filters,
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
    bucket: Option<String>,
    group_by: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Dimension {
    ClientId,
    Upstream,
    Model,
    Protocol,
}

impl Dimension {
    fn column(self) -> &'static str {
        match self {
            Self::ClientId => "client_id",
            Self::Upstream => "upstream_id",
            Self::Model => "model",
            Self::Protocol => "protocol",
        }
    }
}

#[derive(Deserialize)]
pub(super) struct OptionsQuery {
    #[serde(flatten)]
    pub filters: Filters,
    pub dimension: Dimension,
    pub source: Option<String>,
    from: Option<String>,
    to: Option<String>,
    pub q: Option<String>,
    pub limit: Option<String>,
}

impl OptionsQuery {
    pub fn session_column(&self) -> Result<Dimension, Error> {
        match self.dimension {
            Dimension::ClientId | Dimension::Upstream => Ok(self.dimension),
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    query: String,
    snapshot: i64,
    finished: i64,
    seq: i64,
}

pub(super) fn limit(value: Option<&str>, default: usize) -> Result<usize, Error> {
    value.map_or(Ok(default), |value| {
        value
            .parse()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or(Error::Invalid)
    })
}

pub(super) fn encode_cursor<T: Serialize>(cursor: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("游标应可序列化"))
}

pub(super) fn decode_cursor<T: serde::de::DeserializeOwned>(cursor: &str) -> Result<T, Error> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| Error::Invalid)?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
}

pub(super) struct Event {
    request: RequestStatus,
    finished: i64,
    observed: ObservedUsage,
}

impl Event {
    pub fn new(request: RequestStatus, observed: ObservedUsage) -> Self {
        Self {
            request,
            finished: Utc::now().timestamp_millis(),
            observed,
        }
    }

    pub(super) fn snapshot(&self) -> (i64, Value) {
        (
            self.finished,
            request_snapshot(
                &self.request,
                Some(self.finished),
                Some(self.observed.usage),
            ),
        )
    }
}

pub(super) fn request_snapshot(
    request: &RequestStatus,
    finished: Option<i64>,
    usage: Option<Usage>,
) -> Value {
    let phase = match (finished, request.first_byte_ms) {
        (None, Some(_)) => "responding",
        _ => phase_name(request.phase),
    };
    json!({
        "id": request.id,
        "client_id": request.client_id,
        "session_id": request.session_id,
        "upstream": request.upstream,
        "protocol": request.protocol,
        "model": request.model,
        "phase": phase,
        "started_at": timestamp(DateTime::<Utc>::from(request.started_at).timestamp_millis()),
        "finished_at": finished.and_then(timestamp),
        "first_byte_ms": request.first_byte_ms,
        "duration_ms": request.duration_ms,
        "upstream_status": request.upstream_status,
        "usage": usage,
    })
}

pub(super) struct History {
    db: Connection,
    data_from: i64,
}

impl History {
    pub fn open(path: Option<&Path>, legacy: &str) -> io::Result<(Self, String)> {
        let db = match path {
            Some(path) => {
                OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .mode(0o600)
                    .open(path)?;
                Connection::open(path)
            }
            None => Connection::open_in_memory(),
        }
        .map_err(io::Error::other)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(io::Error::other)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .and_then(|()| db.execute_batch(include_str!("schema.sql")))
            .map_err(io::Error::other)?;
        db.execute(
            "INSERT OR IGNORE INTO usage_meta VALUES(1, ?, ?)",
            params![Utc::now().timestamp_millis(), legacy],
        )
        .map_err(io::Error::other)?;
        let (data_from, legacy) = db
            .query_row(
                "SELECT data_from_ms, legacy_json FROM usage_meta WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(io::Error::other)?;
        Ok((Self { db, data_from }, legacy))
    }

    pub fn totals(
        &self,
    ) -> rusqlite::Result<Vec<(Uuid, String, Option<String>, super::protocol::UsageTotal)>> {
        let mut statement = self.db.prepare(
            "SELECT client_id, upstream_id, session_id, coalesce(sum(input_reported_tokens),0),
             coalesce(sum(output_tokens),0), count(*), sum(incomplete)
             FROM usage_event GROUP BY client_id, upstream_id, session_id",
        )?;
        statement
            .query_map([], |row| {
                let id: String = row.get(0)?;
                Ok((
                    Uuid::parse_str(&id).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    row.get(1)?,
                    row.get(2)?,
                    super::protocol::UsageTotal {
                        input_tokens: row.get::<_, i64>(3)? as u64,
                        output_tokens: row.get::<_, i64>(4)? as u64,
                        requests: row.get::<_, i64>(5)? as u64,
                        incomplete_requests: row.get::<_, i64>(6)? as u64,
                    },
                ))
            })?
            .collect()
    }

    pub fn write(&mut self, events: &[Event]) -> rusqlite::Result<()> {
        let transaction = self.db.transaction()?;
        {
            let mut insert = transaction.prepare_cached(
                "INSERT INTO usage_event (request_id, client_id, session_id, upstream_id, protocol,
                 model, started_at_ms, finished_at_ms, report_day, outcome, upstream_status, first_byte_ms,
                 duration_ms, input_reported_tokens, input_uncached_tokens, cache_read_tokens,
                 cache_write_tokens, output_tokens, incomplete, raw_usage_json, normalization_version)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,1)
                 ON CONFLICT(request_id) DO NOTHING",
            )?;
            for Event {
                request,
                finished,
                observed,
            } in events
            {
                let usage = observed.usage;
                let raw = Value::Object(observed.raw.clone());
                let (uncached, cache_read, cache_write) = match request.protocol {
                    Protocol::Codex => {
                        let cached = raw["input_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .or_else(|| raw["prompt_tokens_details"]["cached_tokens"].as_u64());
                        (
                            usage
                                .input_tokens
                                .zip(cached)
                                .and_then(|(input, cached)| input.checked_sub(cached)),
                            cached,
                            Some(0),
                        )
                    }
                    Protocol::Claude => (
                        usage.input_tokens,
                        raw["cache_read_input_tokens"].as_u64(),
                        raw["cache_creation_input_tokens"].as_u64(),
                    ),
                };
                insert.execute(params![
                    request.id.to_string(),
                    request.client_id.to_string(),
                    request.session_id,
                    request.upstream,
                    protocol_name(request.protocol),
                    request.model.as_deref().unwrap_or(""),
                    DateTime::<Utc>::from(request.started_at).timestamp_millis(),
                    finished,
                    day(*finished)
                        .expect("系统时间应可转换为报表日期")
                        .to_string(),
                    phase_name(request.phase),
                    request.upstream_status,
                    sql_count(request.first_byte_ms),
                    sql_count(request.duration_ms).unwrap_or_default(),
                    sql_count(usage.input_tokens),
                    sql_count(uncached),
                    sql_count(cache_read),
                    sql_count(cache_write),
                    sql_count(usage.output_tokens),
                    !usage.complete,
                    (!observed.raw.is_empty()).then(|| raw.to_string())
                ])?;
            }
        }
        transaction.commit()
    }

    pub fn merge(&mut self) -> rusqlite::Result<()> {
        let transaction = self.db.transaction()?;
        let through: i64 = transaction.query_row(
            "SELECT through_seq FROM usage_checkpoint WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        let upper: Option<i64> = transaction.query_row(
            "SELECT max(seq) FROM (SELECT seq FROM usage_event WHERE seq>? ORDER BY seq LIMIT 10000)",
            [through], |r| r.get(0))?;
        if let Some(upper) = upper {
            let assignments = COUNTERS
                .iter()
                .map(|name| format!("{name}={name}+excluded.{name}"))
                .collect::<Vec<_>>()
                .join(",");
            transaction.execute(
                &format!(
                    "INSERT INTO usage_daily SELECT {DIMENSIONS}, {SUM_EVENTS} FROM usage_event
                 WHERE seq>? AND seq<=? GROUP BY {DIMENSIONS}
                 ON CONFLICT({DIMENSIONS}) DO UPDATE SET {assignments}"
                ),
                params![through, upper],
            )?;
            transaction.execute(
                "UPDATE usage_checkpoint SET through_seq=? WHERE id=1",
                [upper],
            )?;
        }
        transaction.commit()
    }

    pub(super) fn recent(&self, limit: usize) -> Result<Vec<(i64, Value)>, Error> {
        let mut stmt = self.db.prepare_cached(&format!(
            "SELECT {DETAIL} FROM usage_event ORDER BY finished_at_ms DESC, seq DESC LIMIT ?"
        ))?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                let mut item = detail(row)?;
                let fields = item.as_object_mut().expect("请求详情应为对象");
                fields.insert("phase".into(), row.get::<_, String>(8)?.into());
                fields.remove("outcome");
                fields.insert(
                    "usage".into(),
                    json!({
                        "input_tokens": row.get::<_, Option<i64>>(12)?,
                        "output_tokens": row.get::<_, Option<i64>>(16)?,
                        "complete": !row.get::<_, bool>(17)?,
                    }),
                );
                Ok((row.get(7)?, item))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn requests(&self, query: RequestsQuery, storage_error: bool) -> Result<Value, Error> {
        query.filters.validate()?;
        let page_size = limit(query.limit.as_deref(), 50)?;
        let fingerprint = {
            use sha2::{Digest, Sha256};
            let mut normalized = serde_json::to_value(&query).map_err(|_| Error::Invalid)?;
            normalized
                .as_object_mut()
                .expect("查询应为对象")
                .remove("limit");
            format!("{:x}", Sha256::digest(normalized.to_string()))
        };
        let cursor = query
            .cursor
            .as_deref()
            .map(decode_cursor::<Cursor>)
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.query != fingerprint || cursor.seq > cursor.snapshot)
        {
            return Err(Error::Invalid);
        }
        let snapshot = if let Some(cursor) = &cursor {
            cursor.snapshot
        } else {
            self.db
                .query_row("SELECT coalesce(max(seq),0) FROM usage_event", [], |r| {
                    r.get(0)
                })?
        };
        let mut conditions = Conditions::default();
        query.filters.apply(&mut conditions);
        apply_times(&mut conditions, query.from.as_deref(), query.to.as_deref())?;
        if let Some(outcome) = query.outcome {
            if !matches!(outcome.as_str(), "completed" | "failed" | "cancelled") {
                return Err(Error::Invalid);
            }
            conditions.bind("outcome = ?", outcome);
        }
        if let Some(incomplete) = query.incomplete {
            let incomplete: bool = incomplete.parse().map_err(|_| Error::Invalid)?;
            conditions.bind("incomplete = ?", i64::from(incomplete));
        }
        conditions.bind("seq <= ?", snapshot);
        if let Some(cursor) = cursor {
            conditions.bind("(finished_at_ms, seq) < (?, ?)", cursor.finished);
            conditions.values.push(cursor.seq.into());
        }
        let sql = format!(
            "SELECT {DETAIL}, seq FROM usage_event{} ORDER BY finished_at_ms DESC, seq DESC LIMIT ?",
            conditions.sql()
        );
        conditions.values.push((page_size as i64 + 1).into());
        let mut stmt = self.db.prepare(&sql)?;
        let mut rows = stmt
            .query_map(params_from_iter(conditions.values), |row| {
                Ok((detail(row)?, row.get::<_, i64>(7)?, row.get::<_, i64>(18)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_cursor = if rows.len() > page_size {
            rows.pop();
            let (_, finished, seq) = rows.last().expect("非空分页");
            Some(encode_cursor(&Cursor {
                query: fingerprint,
                snapshot,
                finished: *finished,
                seq: *seq,
            }))
        } else {
            None
        };
        let items: Vec<_> = rows.into_iter().map(|(detail, _, _)| detail).collect();
        Ok(
            json!({"api_version":API_VERSION,"items":items,"next_cursor":next_cursor,"storage_error":storage_error}),
        )
    }

    pub fn request(&self, id: Uuid, storage_error: bool) -> Result<Value, Error> {
        self.db.query_row(&format!("SELECT {DETAIL}, raw_usage_json FROM usage_event WHERE request_id=?"),
            [id.to_string()], |row| {
                let raw = row.get::<_, Option<String>>(18)?.and_then(|value| serde_json::from_str::<Value>(&value).ok());
                Ok(json!({"api_version":API_VERSION,"item":detail(row)?,"raw_usage":raw,"storage_error":storage_error}))
            }).optional()?.ok_or(Error::NotFound)
    }

    pub fn summary(&self, query: SummaryQuery, storage_error: bool) -> Result<Value, Error> {
        query.filters.validate()?;
        let today = day(Utc::now().timestamp_millis()).ok_or(Error::Invalid)?;
        let (from, to) = match (query.from_date, query.to_date) {
            (None, None) => (
                today
                    .checked_sub_days(Days::new(29))
                    .ok_or(Error::Invalid)?,
                today.succ_opt().ok_or(Error::Invalid)?,
            ),
            (Some(from), Some(to)) if from < to => (from, to),
            _ => return Err(Error::Invalid),
        };
        let bucket = query.bucket.as_deref().unwrap_or("day");
        let period = match bucket {
            "day" => "report_day",
            "month" => "substr(report_day,1,7)||'-01'",
            _ => return Err(Error::Invalid),
        };
        let group = match query.group_by.as_deref() {
            None => "NULL",
            Some("client_id") => "client_id",
            Some("upstream") => "upstream_id",
            Some("protocol") => "protocol",
            Some("model") => "nullif(model,'')",
            _ => return Err(Error::Invalid),
        };
        let mut conditions = Conditions::default();
        query.filters.apply(&mut conditions);
        conditions.bind("report_day >= ?", from.to_string());
        conditions.bind("report_day < ?", to.to_string());
        let conditions_sql = conditions.sql();
        let mut values = conditions.values.clone();
        let branches = if query.filters.session_id.is_some() {
            format!(
                "SELECT {DIMENSIONS}, {SUM_EVENTS} FROM usage_event{conditions_sql} GROUP BY {DIMENSIONS}"
            )
        } else {
            values.extend(conditions.values);
            format!("SELECT * FROM usage_daily{conditions_sql} UNION ALL
              SELECT {DIMENSIONS}, {SUM_EVENTS} FROM usage_event{conditions_sql}
              AND seq > (SELECT through_seq FROM usage_checkpoint WHERE id=1) GROUP BY {DIMENSIONS}")
        };
        let sums = COUNTERS
            .iter()
            .map(|name| format!("sum({name})"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "WITH source AS ({branches}) SELECT {period} AS period, {group} AS dimension, {sums}
                          FROM source GROUP BY period, dimension ORDER BY period, dimension"
        );
        let mut stmt = self.db.prepare(&sql)?;
        let mut totals = [0_u64; 7];
        let rows = stmt
            .query_map(params_from_iter(values), |row| {
                let mut item = serde_json::Map::new();
                item.insert("period_start".into(), json!(row.get::<_, String>(0)?));
                item.insert("group".into(), json!(row.get::<_, Option<String>>(1)?));
                for (index, name) in COUNTERS.iter().enumerate() {
                    let value = row.get::<_, i64>(index + 2)? as u64;
                    totals[index] = totals[index].saturating_add(value);
                    item.insert((*name).into(), json!(value));
                }
                item.remove("input_reported_tokens");
                Ok(item)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let totals: serde_json::Map<_, _> = COUNTERS
            .iter()
            .zip(totals)
            .filter(|(name, _)| **name != "input_reported_tokens")
            .map(|(name, value)| ((*name).into(), json!(value)))
            .collect();
        Ok(
            json!({"api_version":API_VERSION,"time_zone":ZONE,"from_date":from.to_string(),
            "to_date":to.to_string(),"data_from":timestamp(self.data_from),"bucket":bucket,
            "group_by":query.group_by,"items":rows,"totals":totals,"storage_error":storage_error}),
        )
    }

    pub fn options(&self, query: OptionsQuery) -> Result<Value, Error> {
        query.filters.validate()?;
        if matches!(query.dimension, Dimension::Protocol)
            || !matches!(query.source.as_deref(), None | Some("history"))
        {
            return Err(Error::Invalid);
        }
        let page_size = limit(query.limit.as_deref(), 30)?;
        let column = query.dimension.column();
        let mut conditions = Conditions::default();
        query.filters.apply(&mut conditions);
        apply_times(&mut conditions, query.from.as_deref(), query.to.as_deref())?;
        conditions.parts.push(format!("{column} <> ''"));
        if let Some(prefix) = query.q {
            conditions.bind(&format!("instr({column}, ?) = 1"), prefix);
        }
        let sql = format!(
            "SELECT DISTINCT {column} FROM usage_event{} ORDER BY {column} LIMIT ?",
            conditions.sql()
        );
        conditions.values.push((page_size as i64 + 1).into());
        let mut stmt = self.db.prepare(&sql)?;
        let values = stmt
            .query_map(params_from_iter(conditions.values), |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(option_report(values, page_size))
    }
}

pub(super) fn option_report(values: Vec<String>, limit: usize) -> Value {
    let more = values.len() > limit;
    let items: Vec<_> = values
        .into_iter()
        .take(limit)
        .map(|value| json!({"value":value}))
        .collect();
    json!({"api_version":API_VERSION,"items":items,"has_more":more})
}

fn apply_times(
    conditions: &mut Conditions,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(), Error> {
    let parse = |value: &str| {
        DateTime::parse_from_rfc3339(value)
            .ok()
            .filter(|time| time.timestamp_subsec_nanos() % 1_000_000 == 0)
            .map(|time| time.timestamp_millis())
            .ok_or(Error::Invalid)
    };
    let from = from.map(parse).transpose()?;
    let to = to.map(parse).transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from >= to) {
        return Err(Error::Invalid);
    }
    if let Some(from) = from {
        conditions.bind("finished_at_ms >= ?", from);
    }
    if let Some(to) = to {
        conditions.bind("finished_at_ms < ?", to);
    }
    Ok(())
}

fn timestamp(ms: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn day(ms: i64) -> Option<NaiveDate> {
    // 代理记录使用当前东方八区报表时区。
    let offset = FixedOffset::east_opt(8 * 3600)?;
    DateTime::<Utc>::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&offset).date_naive())
}

fn sql_count(value: Option<u64>) -> Option<i64> {
    value.and_then(|value| i64::try_from(value).ok())
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Codex => "codex",
        Protocol::Claude => "claude",
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Waiting => "waiting",
        Phase::Completed => "completed",
        Phase::Failed => "failed",
        Phase::Cancelled => "cancelled",
    }
}

fn detail(row: &Row) -> rusqlite::Result<Value> {
    let model: String = row.get(5)?;
    Ok(json!({
        "id":row.get::<_,String>(0)?,"client_id":row.get::<_,String>(1)?,
        "session_id":row.get::<_,Option<String>>(2)?,"upstream":row.get::<_,String>(3)?,
        "protocol":row.get::<_,String>(4)?,"model":(!model.is_empty()).then_some(model),
        "started_at":timestamp(row.get(6)?),"finished_at":timestamp(row.get(7)?),
        "outcome":row.get::<_,String>(8)?,"upstream_status":row.get::<_,Option<u16>>(9)?,
        "first_byte_ms":row.get::<_,Option<i64>>(10)?,"duration_ms":row.get::<_,i64>(11)?,
        "usage":{"input_reported_tokens":row.get::<_,Option<i64>>(12)?,
            "input_uncached_tokens":row.get::<_,Option<i64>>(13)?,"cache_read_tokens":row.get::<_,Option<i64>>(14)?,
            "cache_write_tokens":row.get::<_,Option<i64>>(15)?,"output_tokens":row.get::<_,Option<i64>>(16)?,
            "complete":!row.get::<_,bool>(17)?}
    }))
}
