CREATE TABLE IF NOT EXISTS usage_event (
    seq INTEGER PRIMARY KEY,
    request_id TEXT NOT NULL UNIQUE,
    client_id TEXT NOT NULL,
    session_id TEXT,
    upstream_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    model TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER NOT NULL,
    report_day TEXT NOT NULL,
    outcome TEXT NOT NULL,
    upstream_status INTEGER,
    first_byte_ms INTEGER,
    duration_ms INTEGER NOT NULL,
    input_reported_tokens INTEGER,
    input_uncached_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    output_tokens INTEGER,
    incomplete INTEGER NOT NULL CHECK (incomplete IN (0, 1)),
    raw_usage_json TEXT,
    normalization_version INTEGER NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS usage_event_time
    ON usage_event (finished_at_ms DESC, seq DESC);

CREATE INDEX IF NOT EXISTS usage_event_client_upstream_time
    ON usage_event (client_id, upstream_id, finished_at_ms DESC, seq DESC);

CREATE INDEX IF NOT EXISTS usage_event_client_session_time
    ON usage_event (client_id, session_id, finished_at_ms DESC, seq DESC)
    WHERE session_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS usage_daily (
    report_day TEXT NOT NULL,
    client_id TEXT NOT NULL,
    upstream_id TEXT NOT NULL,
    protocol TEXT NOT NULL,
    model TEXT NOT NULL,
    requests INTEGER NOT NULL,
    incomplete_requests INTEGER NOT NULL,
    input_reported_tokens INTEGER NOT NULL,
    input_uncached_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    PRIMARY KEY (report_day, client_id, upstream_id, protocol, model)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS usage_checkpoint (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    through_seq INTEGER NOT NULL
) STRICT;

INSERT OR IGNORE INTO usage_checkpoint (id, through_seq) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS usage_meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data_from_ms INTEGER NOT NULL,
    legacy_json TEXT NOT NULL
) STRICT;
