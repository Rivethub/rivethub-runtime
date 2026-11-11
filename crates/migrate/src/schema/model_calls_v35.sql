CREATE TABLE IF NOT EXISTS rh_model_calls (
    call_id TEXT NOT NULL CHECK (call_id <> ''),
    owner_user_id TEXT NOT NULL,
    session_key TEXT,
    turn_id TEXT,
    request_kind TEXT,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    provider TEXT NOT NULL,
    model_requested TEXT,
    model_served TEXT,
    account TEXT,
    status_code INTEGER,
    outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'rate_limited', 'client_error', 'server_error', 'aborted')),
    retry_after_secs INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    PRIMARY KEY (owner_user_id, call_id)
);
CREATE INDEX IF NOT EXISTS rh_model_calls_session ON rh_model_calls (session_key, started_at);
