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

ALTER TABLE ros_wiki_topics ADD COLUMN visibility TEXT NOT NULL DEFAULT 'private';
ALTER TABLE ros_wiki_topics ADD COLUMN scope TEXT REFERENCES rh_wiki_scopes (scope);
ALTER TABLE ros_wiki_topics ADD COLUMN local_slug TEXT;
ALTER TABLE rh_topic_pages ADD COLUMN sections_were_null INTEGER NOT NULL DEFAULT 0
    CHECK (sections_were_null IN (0, 1));
ALTER TABLE ros_wiki_redirects ADD COLUMN owner_user_id TEXT;
ALTER TABLE ros_tag_taxonomy ADD COLUMN owner_user_id TEXT;
ALTER TABLE ros_tag_taxonomy ADD COLUMN globally_shared INTEGER CHECK (globally_shared = 1);
ALTER TABLE rh_topic_tombstones ADD COLUMN owner_user_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS rh_wiki_topics_scope_local ON ros_wiki_topics (scope, local_slug);
ALTER TABLE rh_topic_tombstones ADD COLUMN locally_deleted INTEGER NOT NULL DEFAULT 0
    CHECK (locally_deleted IN (0, 1));
