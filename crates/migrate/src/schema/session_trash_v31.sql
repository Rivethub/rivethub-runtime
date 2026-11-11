CREATE TABLE IF NOT EXISTS rh_session_trash (
    session_key TEXT PRIMARY KEY CHECK (session_key <> ''),
    trashed_at TEXT NOT NULL,
    trashed_by TEXT
);
CREATE INDEX IF NOT EXISTS rh_session_trash_at ON rh_session_trash (trashed_at);
