CREATE TABLE IF NOT EXISTS rh_entity_facts (
    id TEXT PRIMARY KEY,
    topic_id TEXT NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (key <> '' AND length(key) <= 64),
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rh_entity_facts_key ON rh_entity_facts (topic_id, key, created_at);
CREATE TABLE IF NOT EXISTS rh_entity_fact_claims (
    id TEXT PRIMARY KEY,
    fact_id TEXT NOT NULL REFERENCES rh_entity_facts (id) ON DELETE CASCADE,
    value TEXT NOT NULL CHECK (value <> '' AND length(value) <= 400),
    actor TEXT NOT NULL CHECK (actor IN ('you', 'wiki-writer')),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    session_id TEXT CHECK (session_id <> ''),
    summary_id TEXT,
    evidence TEXT CHECK (length(evidence) <= 300),
    at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    CHECK (actor = 'you' OR pinned = 0),
    CHECK ((actor = 'wiki-writer') = (session_id IS NOT NULL AND summary_id IS NOT NULL)),
    CHECK (actor = 'wiki-writer' OR (session_id IS NULL AND summary_id IS NULL AND evidence IS NULL))
);
CREATE INDEX IF NOT EXISTS rh_entity_fact_claims_fact ON rh_entity_fact_claims (fact_id, at);
CREATE INDEX IF NOT EXISTS rh_entity_fact_claims_session ON rh_entity_fact_claims (session_id)
    WHERE session_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS rh_entity_fact_claims_summary ON rh_entity_fact_claims (fact_id, summary_id)
    WHERE summary_id IS NOT NULL;
