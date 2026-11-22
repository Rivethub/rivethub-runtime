CREATE TABLE IF NOT EXISTS rh_entity_extractions (
    summary_id TEXT PRIMARY KEY REFERENCES ros_summaries (id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('done', 'skipped', 'failed')),
    pipeline_version INTEGER NOT NULL,
    entities_touched TEXT NOT NULL DEFAULT '[]',
    git_sha TEXT,
    error TEXT,
    extracted_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rh_entity_extractions_status
    ON rh_entity_extractions (status, extracted_at DESC);

CREATE TABLE IF NOT EXISTS rh_entity_history (
    entity_id TEXT NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
    summary_id TEXT NOT NULL REFERENCES ros_summaries (id) ON DELETE CASCADE,
    conversation_id TEXT,
    session_on TEXT NOT NULL,
    sentence TEXT NOT NULL CHECK (sentence <> ''),
    owner_user_id TEXT NOT NULL,
    PRIMARY KEY (entity_id, summary_id)
);

CREATE TABLE IF NOT EXISTS rh_entity_candidates (
    id TEXT PRIMARY KEY,
    summary_id TEXT NOT NULL REFERENCES ros_summaries (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    type_name TEXT NOT NULL,
    confidence REAL NOT NULL,
    reason TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rh_entity_candidates_summary
    ON rh_entity_candidates (summary_id, created_at DESC);

CREATE TABLE IF NOT EXISTS rh_entity_decisions (
    id TEXT PRIMARY KEY,
    summary_id TEXT NOT NULL REFERENCES ros_summaries (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('entity', 'type')),
    proposed_name TEXT NOT NULL,
    proposed_type TEXT NOT NULL,
    candidates TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('same', 'different', 'none', 'unreachable', 'hold')),
    choice TEXT,
    confidence REAL,
    model TEXT NOT NULL,
    entity_id TEXT,
    created_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rh_entity_decisions_summary
    ON rh_entity_decisions (summary_id, created_at DESC);

CREATE TABLE IF NOT EXISTS rh_entity_embeddings (
    entity_id TEXT PRIMARY KEY REFERENCES rh_topics (id) ON DELETE CASCADE,
    embedding BLOB NOT NULL CHECK (length(embedding) = 4096),
    embedded_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rh_entity_applied (
    entity_id TEXT PRIMARY KEY REFERENCES rh_topics (id) ON DELETE CASCADE,
    applied_through TEXT NOT NULL,
    summary_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rh_entity_type_seen (
    entity_id TEXT NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
    summary_id TEXT NOT NULL REFERENCES ros_summaries (id) ON DELETE CASCADE,
    type_name TEXT NOT NULL CHECK (type_name <> ''),
    observed_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    PRIMARY KEY (entity_id, summary_id, type_name)
);
CREATE INDEX IF NOT EXISTS rh_entity_type_seen_recent
    ON rh_entity_type_seen (entity_id, observed_at DESC, summary_id DESC);

INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' || substr(hex(randomblob(2)), 2)
             || '-' || substr('89ab', 1 + (abs(random()) % 4), 1) || substr(hex(randomblob(2)), 2)
             || '-' || hex(randomblob(6))),
       'type', seed.column1, seed.column2,
       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  FROM (VALUES ('organizations', 'Organizations'), ('projects', 'Projects'), ('places', 'Places')) AS seed
 WHERE NOT EXISTS (SELECT 1 FROM rh_topics t WHERE t.kind = 'type' AND t.slug = seed.column1);

CREATE TABLE IF NOT EXISTS rh_entity_leases (
    entity_id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
