CREATE TABLE IF NOT EXISTS rh_worker_pauses (
    role TEXT PRIMARY KEY CHECK (role IN ('categorizer', 'wiki-builder', 'entity-builder')),
    paused_at TEXT NOT NULL,
    paused_by TEXT
);

CREATE TABLE IF NOT EXISTS rh_tasks_archive (
    id TEXT PRIMARY KEY,
    origin TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS rh_tasks_archive_id ON rh_tasks_archive(id);
