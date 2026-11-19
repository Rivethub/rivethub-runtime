-- Runtime path: crates/migrate/migrations/0076_top_down_summaries.sql.
-- Additive, no transcript scan or rewrite. Same transaction and marker as migration 75.
DO $rivet$
DECLARE
    relation text;
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0076_top_down_summaries.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    ALTER TABLE rh_summaries
        ADD COLUMN summary_ordinal integer,
        ADD COLUMN index_needs_repair boolean NOT NULL DEFAULT false,
        ADD COLUMN session_root_for uuid GENERATED ALWAYS AS (
            CASE WHEN kind = 'session' THEN conversation_id END) STORED,
        ADD CONSTRAINT rh_summary_one_session UNIQUE (session_root_for) DEFERRABLE INITIALLY DEFERRED,
        ADD COLUMN issued_part_ordinal integer,
        ADD COLUMN input_token_estimate bigint,
        ADD COLUMN source_first integer,
        ADD COLUMN source_last integer,
        ADD COLUMN range_verified boolean,
        ADD COLUMN input_key bytea CHECK (octet_length(input_key) = 32),
        ADD COLUMN index_key bytea CHECK (octet_length(index_key) = 32),
        ADD COLUMN quality_flags integer NOT NULL DEFAULT 0 CHECK (quality_flags >= 0),
        ADD COLUMN section_char_start integer,
        ADD COLUMN section_char_end integer,
        ADD CONSTRAINT rh_summary_v7_shape CHECK (
          (kind IN ('leaf', 'branch', 'root') AND pipeline_version < 7)
          OR (pipeline_version = 7 AND conversation_id IS NOT NULL
            AND summary_ordinal IS NOT NULL AND summary_ordinal >= 0
            AND source_first IS NOT NULL AND source_first > 0
            AND source_last IS NOT NULL AND source_last >= source_first
            AND input_key IS NOT NULL AND octet_length(input_key) = 32
            AND message_count > 0 AND earliest_at IS NOT NULL AND latest_at IS NOT NULL
            AND latest_at >= earliest_at
            AND CASE kind
              WHEN 'session' THEN depth = 2 AND parent_id IS NULL AND summary_ordinal = 0
                AND source_first = 1 AND range_verified IS NULL
                AND section_char_start IS NULL AND section_char_end IS NULL
              WHEN 'part' THEN depth = 1 AND parent_id IS NOT NULL AND summary_ordinal > 0
                AND issued_part_ordinal IS NOT NULL AND issued_part_ordinal > 0
                AND input_token_estimate IS NOT NULL AND input_token_estimate > 0
                AND range_verified IS NULL AND section_char_start IS NULL AND section_char_end IS NULL
              WHEN 'section' THEN depth = 0 AND parent_id IS NOT NULL AND range_verified IS NOT NULL
                AND section_char_start IS NOT NULL AND section_char_start >= 0
                AND section_char_end IS NOT NULL AND section_char_end > section_char_start
              ELSE false
            END)
        ) NOT VALID;
    ALTER TABLE rh_summary_sources
        ADD COLUMN render_key bytea CHECK (octet_length(render_key) = 32);
    CREATE INDEX rh_summary_sources_order ON rh_summary_sources (summary_id, ordinal);

    CREATE TABLE rh_summary_passages (
        id uuid PRIMARY KEY,
        record_id uuid NOT NULL REFERENCES rh_summaries (id) ON DELETE CASCADE,
        ordinal integer NOT NULL CHECK (ordinal >= 0),
        char_start integer NOT NULL CHECK (char_start >= 0),
        char_end integer NOT NULL,
        content text NOT NULL CHECK (char_length(content) BETWEEN 1 AND 6000),
        earliest_at timestamptz NOT NULL,
        latest_at timestamptz NOT NULL,
        content_tsv tsvector GENERATED ALWAYS AS (to_tsvector('english', content)) STORED,
        index_key bytea NOT NULL CHECK (octet_length(index_key) = 32),
        embedding halfvec(1024),
        CHECK (char_end - char_start = char_length(content) AND latest_at >= earliest_at),
        UNIQUE (record_id, ordinal)
    );
    CREATE INDEX rh_summary_passages_fts ON rh_summary_passages USING gin (content_tsv);
    CREATE INDEX rh_summary_passages_trgm ON rh_summary_passages USING gin (content gin_trgm_ops);
    CREATE INDEX rh_summary_passages_hnsw ON rh_summary_passages USING hnsw (embedding halfvec_cosine_ops);
    CREATE INDEX rh_summary_passages_source_time ON rh_summary_passages (latest_at DESC);
    CREATE POLICY rh_read ON rh_summary_passages FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_summaries s WHERE s.id = rh_summary_passages.record_id));
    CREATE POLICY rh_write ON rh_summary_passages USING ((SELECT rh_scope_maintenance()));
    ALTER TABLE rh_summary_passages ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_summary_passages FORCE ROW LEVEL SECURITY;

    CREATE TABLE rh_summary_state (
        conversation_id uuid PRIMARY KEY REFERENCES rh_conversations (id) ON DELETE CASCADE,
        input_revision bigint NOT NULL CHECK (input_revision >= 0),
        last_input_at timestamptz NOT NULL,
        unsettled_since timestamptz,
        settled_at timestamptz,
        summarized_through bigint CHECK (summarized_through BETWEEN 0 AND input_revision),
        failed_revision bigint CHECK (failed_revision BETWEEN 0 AND input_revision),
        historical_only boolean NOT NULL,
        last_observed_event_at timestamptz,
        last_observed_end_at timestamptz,
        last_observed_end_key text,
        CHECK ((unsettled_since IS NULL) <> (settled_at IS NULL))
    );
    CREATE INDEX rh_summary_idle_due ON rh_summary_state (last_input_at, conversation_id)
        WHERE settled_at IS NULL AND NOT historical_only;
    CREATE INDEX rh_summary_settled_due ON rh_summary_state (settled_at, conversation_id)
        WHERE settled_at IS NOT NULL AND NOT historical_only
          AND summarized_through IS DISTINCT FROM input_revision
          AND failed_revision IS DISTINCT FROM input_revision;

    CREATE TABLE rh_summary_openings (
        conversation_id uuid PRIMARY KEY REFERENCES rh_conversations (id) ON DELETE CASCADE,
        content text NOT NULL CHECK (char_length(content) BETWEEN 1 AND 2000),
        filing_armed boolean NOT NULL DEFAULT false,
        title_armed boolean NOT NULL DEFAULT false
    );
    CREATE TABLE rh_summary_runs (
        id uuid PRIMARY KEY,
        conversation_id uuid NOT NULL UNIQUE REFERENCES rh_conversations (id) ON DELETE CASCADE,
        revision bigint NOT NULL CHECK (revision >= 0),
        expected_root uuid, -- Compare-only. No FK that could pin an old tree.
        manifest jsonb NOT NULL,
        created_at timestamptz NOT NULL DEFAULT now()
    );
    CREATE TABLE rh_summary_checkpoints (
        run_id uuid NOT NULL REFERENCES rh_summary_runs (id) ON DELETE CASCADE,
        artifact_key bytea NOT NULL CHECK (octet_length(artifact_key) = 32),
        artifact jsonb NOT NULL,
        PRIMARY KEY (run_id, artifact_key)
    );
    CREATE TABLE rh_summary_deliveries (
        conversation_id uuid NOT NULL REFERENCES rh_conversations (id) ON DELETE CASCADE,
        consumer text NOT NULL CHECK (consumer IN ('wiki', 'entity')),
        message_id uuid NOT NULL REFERENCES rh_messages (id) ON DELETE CASCADE,
        anchor_id uuid REFERENCES rh_summaries (id) DEFERRABLE INITIALLY DEFERRED,
        completed_version integer,
        historical_backlog boolean NOT NULL DEFAULT false,
        PRIMARY KEY (conversation_id, consumer, message_id),
        CHECK (num_nonnulls(anchor_id, completed_version) + historical_backlog::int = 1)
    );
    CREATE INDEX rh_summary_delivery_anchor ON rh_summary_deliveries (anchor_id, consumer)
        WHERE anchor_id IS NOT NULL;
    CREATE INDEX rh_summary_delivery_message ON rh_summary_deliveries (message_id);

    CREATE TABLE rh_summary_conversion (
        singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
        enabled boolean NOT NULL DEFAULT false,
        include_giants boolean NOT NULL DEFAULT false,
        inventory_after uuid,
        inventory_complete boolean NOT NULL DEFAULT false,
        rate bigint NOT NULL DEFAULT 200000 CHECK (rate > 0),
        bucket_tokens double precision NOT NULL DEFAULT 0 CHECK (bucket_tokens >= 0),
        bucket_at timestamptz NOT NULL DEFAULT now(),
        charged_tokens bigint NOT NULL DEFAULT 0 CHECK (charged_tokens >= 0),
        provider_input_tokens bigint NOT NULL DEFAULT 0,
        provider_output_tokens bigint NOT NULL DEFAULT 0
    );
    INSERT INTO rh_summary_conversion DEFAULT VALUES;
    CREATE TABLE rh_summary_conversion_items (
        conversation_id uuid PRIMARY KEY REFERENCES rh_conversations (id) ON DELETE CASCADE,
        last_input_at timestamptz NOT NULL,
        last_used_at timestamptz,
        estimated_tokens bigint NOT NULL CHECK (estimated_tokens >= 0),
        state text NOT NULL CHECK (state IN ('pending','waiting_quiet','reserved','complete','failed')),
        next_at timestamptz NOT NULL DEFAULT now(),
        reservation_expires_at timestamptz,
        last_error text,
        CHECK ((state = 'reserved') = (reservation_expires_at IS NOT NULL))
    );
    CREATE UNIQUE INDEX rh_summary_conversion_one_reserved
        ON rh_summary_conversion_items ((true)) WHERE state = 'reserved';
    CREATE INDEX rh_summary_conversion_pick ON rh_summary_conversion_items
        ((estimated_tokens > 1000000), (greatest(last_input_at, last_used_at)) DESC, conversation_id)
        WHERE state IN ('pending','waiting_quiet');

    FOREACH relation IN ARRAY ARRAY[
        'rh_summary_state', 'rh_summary_openings', 'rh_summary_runs', 'rh_summary_checkpoints',
        'rh_summary_deliveries', 'rh_summary_conversion', 'rh_summary_conversion_items'
    ] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', relation);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', relation);
        EXECUTE format('CREATE POLICY rh_maintenance ON %I USING ((SELECT rh_scope_maintenance()))', relation);
    END LOOP;

    -- Capture is scoped to the conversation owner. Other state writes use the
    -- already-authorized repository maintenance transaction.
    CREATE POLICY rh_summary_state_capture ON rh_summary_state USING (
        EXISTS (SELECT 1 FROM rh_conversations c WHERE c.id = rh_summary_state.conversation_id
                AND c.owner_user_id = (SELECT rh_scope_principal())));
    CREATE POLICY rh_summary_openings_capture ON rh_summary_openings USING (
        EXISTS (SELECT 1 FROM rh_conversations c WHERE c.id = rh_summary_openings.conversation_id
                AND c.owner_user_id = (SELECT rh_scope_principal())));

    -- Portable export follows the same conversation visibility as the tree.
    -- Receipt writes are reserved for maintenance.
    CREATE POLICY rh_summary_deliveries_read ON rh_summary_deliveries FOR SELECT USING (
        EXISTS (SELECT 1 FROM rh_conversations c WHERE c.id = rh_summary_deliveries.conversation_id));

    -- Bounded direct link resolver. Legacy rollup-remap's recursive union lives
    -- only in the replacement writer. Read RLS remains security invoker.
    CREATE FUNCTION rh_summary_message_ids(node_id uuid)
    RETURNS TABLE (message_id uuid, ordinal integer)
    LANGUAGE sql STABLE SECURITY INVOKER AS $resolver$
        SELECT link.message_id, link.ordinal
          FROM rh_summaries node
          JOIN rh_summary_sources link ON link.summary_id =
            CASE WHEN node.kind = 'section' THEN node.parent_id ELSE node.id END
         WHERE node.id = node_id
           AND (node.kind <> 'section' OR link.ordinal BETWEEN node.source_first AND node.source_last)
    $resolver$;

    CREATE FUNCTION rh_summary_record_indexed(node_id uuid)
    RETURNS boolean LANGUAGE sql STABLE SECURITY INVOKER AS $indexed$
        SELECT coalesce(bool_and(embedded AND char_start = previous_end AND ordinal = position)
            AND max(char_end) = (SELECT length(s.content) FROM rh_summaries s WHERE s.id = node_id),
            false)
        FROM (
            SELECT p.char_start, p.char_end, p.ordinal, p.embedding IS NOT NULL AS embedded,
                   lag(p.char_end, 1, 0) OVER (ORDER BY p.ordinal) AS previous_end,
                   row_number() OVER (ORDER BY p.ordinal) - 1 AS position
            FROM rh_summary_passages p WHERE p.record_id = node_id
        ) pieces
    $indexed$;

    -- v7 vectors are part of private preparation. Legacy embed discovery must
    -- also exclude v7 when querying the old whole-vector index.
    DROP TRIGGER trg_embed_summary ON rh_summaries;
    CREATE TRIGGER trg_embed_summary AFTER INSERT ON rh_summaries
        FOR EACH ROW WHEN (NEW.pipeline_version < 7) EXECUTE FUNCTION notify_embedding_queue();

    INSERT INTO _rivetos_migrations (name) VALUES ('0076_top_down_summaries.sql');
END
$rivet$;
