CREATE TABLE IF NOT EXISTS rh_wiki_scopes (
    scope TEXT PRIMARY KEY NOT NULL,
    owner_user_id TEXT NOT NULL,
    visibility TEXT NOT NULL,
    key_prefix TEXT NOT NULL,
    directory TEXT NOT NULL UNIQUE,
    UNIQUE (owner_user_id, visibility)
);

CREATE TABLE IF NOT EXISTS rh_agent_presets (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    color TEXT NOT NULL DEFAULT '',
    harness_id TEXT,
    model TEXT NOT NULL DEFAULT '',
    effort TEXT NOT NULL DEFAULT '',
    system_prompt TEXT NOT NULL DEFAULT '',
    node TEXT NOT NULL,
    directory TEXT NOT NULL,
    shared_link INTEGER NOT NULL DEFAULT 1 CHECK (shared_link IN (0, 1)),
    node_base_url TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    sort_order INTEGER,
    owner_user_id TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS rh_agent_presets_name ON rh_agent_presets (lower(name));
