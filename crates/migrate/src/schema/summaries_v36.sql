-- SQLite upgrade V35 -> V36, in upgrade.rs::step's transaction.
-- Keep legacy raw/summary FTS tables. v7 readers admit session/part as whole
-- hits; section FTS rows are never candidates. No summary passage table.
ALTER TABLE ros_summaries ADD COLUMN summary_ordinal INTEGER;
ALTER TABLE ros_summaries ADD COLUMN index_needs_repair INTEGER NOT NULL DEFAULT 0
    CHECK (index_needs_repair IN (0, 1));
ALTER TABLE ros_summaries ADD COLUMN issued_part_ordinal INTEGER;
ALTER TABLE ros_summaries ADD COLUMN input_token_estimate INTEGER;
ALTER TABLE ros_summary_sources ADD COLUMN render_key BLOB CHECK (length(render_key) = 32);
ALTER TABLE ros_summaries ADD COLUMN source_first INTEGER;
ALTER TABLE ros_summaries ADD COLUMN source_last INTEGER;
ALTER TABLE ros_summaries ADD COLUMN range_verified INTEGER CHECK (range_verified IN (0, 1));
ALTER TABLE ros_summaries ADD COLUMN input_key BLOB CHECK (length(input_key) = 32);
ALTER TABLE ros_summaries ADD COLUMN index_key BLOB CHECK (length(index_key) = 32);
ALTER TABLE ros_summaries ADD COLUMN quality_flags INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ros_summaries ADD COLUMN section_char_start INTEGER;
ALTER TABLE ros_summaries ADD COLUMN section_char_end INTEGER;
CREATE UNIQUE INDEX rh_summary_one_session ON ros_summaries (conversation_id) WHERE kind = 'session';
CREATE INDEX rh_summary_sources_order ON ros_summary_sources (summary_id, ordinal);

CREATE TABLE rh_summary_state (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES ros_conversations(id) ON DELETE CASCADE,
    input_revision INTEGER NOT NULL CHECK (input_revision >= 0),
    last_input_at TEXT NOT NULL,
    unsettled_since TEXT,
    settled_at TEXT,
    summarized_through INTEGER CHECK (summarized_through BETWEEN 0 AND input_revision),
    failed_revision INTEGER CHECK (failed_revision BETWEEN 0 AND input_revision),
    historical_only INTEGER NOT NULL CHECK (historical_only IN (0,1)),
    last_observed_event_at TEXT,
    last_observed_end_at TEXT,
    last_observed_end_key TEXT,
    CHECK ((unsettled_since IS NULL) <> (settled_at IS NULL))
);
CREATE INDEX rh_summary_idle_due ON rh_summary_state (last_input_at, conversation_id)
    WHERE settled_at IS NULL AND historical_only = 0;
CREATE INDEX rh_summary_settled_due ON rh_summary_state (settled_at, conversation_id)
    WHERE settled_at IS NOT NULL AND historical_only = 0
      AND summarized_through IS NOT input_revision AND failed_revision IS NOT input_revision;

CREATE TABLE rh_summary_openings (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES ros_conversations(id) ON DELETE CASCADE,
    content TEXT NOT NULL CHECK (length(content) BETWEEN 1 AND 2000),
    filing_armed INTEGER NOT NULL DEFAULT 0 CHECK (filing_armed IN (0,1)),
    historical_pending INTEGER NOT NULL DEFAULT 0 CHECK (historical_pending IN (0, 1)),
    title_armed INTEGER NOT NULL DEFAULT 0 CHECK (title_armed IN (0,1))
);
CREATE TABLE rh_summary_runs (
    id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL UNIQUE REFERENCES ros_conversations(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    expected_root TEXT,
    manifest TEXT NOT NULL CHECK (json_valid(manifest)),
    created_at TEXT NOT NULL
);
CREATE TABLE rh_summary_checkpoints (
    run_id TEXT NOT NULL REFERENCES rh_summary_runs(id) ON DELETE CASCADE,
    artifact_key BLOB NOT NULL CHECK (length(artifact_key) = 32),
    artifact TEXT NOT NULL CHECK (json_valid(artifact)),
    PRIMARY KEY (run_id, artifact_key)
);
CREATE TABLE ros_summary_deliveries (
    conversation_id TEXT NOT NULL REFERENCES ros_conversations(id) ON DELETE CASCADE,
    consumer TEXT NOT NULL CHECK (consumer IN ('wiki', 'entity')),
    message_id TEXT NOT NULL REFERENCES ros_messages(id) ON DELETE CASCADE,
    anchor_id TEXT REFERENCES ros_summaries(id) DEFERRABLE INITIALLY DEFERRED,
    completed_version INTEGER,
    historical_backlog INTEGER NOT NULL DEFAULT 0 CHECK (historical_backlog IN (0,1)),
    PRIMARY KEY (conversation_id, consumer, message_id),
    CHECK ((anchor_id IS NOT NULL) + (completed_version IS NOT NULL) + historical_backlog = 1)
);
CREATE INDEX rh_summary_delivery_anchor ON ros_summary_deliveries (anchor_id, consumer)
    WHERE anchor_id IS NOT NULL;
CREATE INDEX rh_summary_delivery_message ON ros_summary_deliveries (message_id);

CREATE TABLE rh_summary_conversion (
    singleton INTEGER PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0,1)),
    include_giants INTEGER NOT NULL DEFAULT 0 CHECK (include_giants IN (0,1)),
    inventory_after TEXT,
    inventory_complete INTEGER NOT NULL DEFAULT 0 CHECK (inventory_complete IN (0,1)),
    rate INTEGER NOT NULL DEFAULT 200000 CHECK (rate > 0),
    bucket_tokens REAL NOT NULL DEFAULT 0 CHECK (bucket_tokens >= 0),
    bucket_at TEXT NOT NULL DEFAULT '1970-01-01T00:00:00.000Z',
    charged_tokens INTEGER NOT NULL DEFAULT 0 CHECK (charged_tokens >= 0),
    provider_input_tokens INTEGER NOT NULL DEFAULT 0,
    provider_output_tokens INTEGER NOT NULL DEFAULT 0
);
INSERT INTO rh_summary_conversion DEFAULT VALUES;
CREATE TABLE rh_summary_conversion_items (
    conversation_id TEXT PRIMARY KEY NOT NULL REFERENCES ros_conversations(id) ON DELETE CASCADE,
    last_input_at TEXT NOT NULL,
    last_used_at TEXT,
    estimated_tokens INTEGER NOT NULL CHECK (estimated_tokens >= 0),
    state TEXT NOT NULL CHECK (state IN ('pending','waiting_quiet','reserved','complete','failed')),
    next_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    reservation_expires_at TEXT,
    last_error TEXT,
    CHECK ((state = 'reserved') = (reservation_expires_at IS NOT NULL))
);
CREATE UNIQUE INDEX rh_summary_conversion_one_reserved ON rh_summary_conversion_items ((1))
    WHERE state = 'reserved';
CREATE INDEX rh_summary_conversion_pick ON rh_summary_conversion_items
    ((estimated_tokens > 1000000), (max(last_input_at, coalesce(last_used_at, last_input_at))) DESC,
     conversation_id)
    WHERE state IN ('pending','waiting_quiet');

-- Repository commit checks the same v7 shape, same-owner parent and strictly
-- increasing owning source ordinals. Add no old-table rebuild.
