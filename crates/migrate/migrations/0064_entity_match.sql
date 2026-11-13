DO $rivet$
DECLARE
    tbl text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0064_entity_match.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_entity_candidates (
        id UUID PRIMARY KEY,
        summary_id UUID NOT NULL REFERENCES rh_summaries (id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        type_name TEXT NOT NULL,
        confidence REAL NOT NULL,
        reason TEXT NOT NULL,
        payload JSONB NOT NULL,
        created_at TIMESTAMPTZ NOT NULL,
        owner_user_id TEXT NOT NULL
    );
    CREATE INDEX idx_rh_entity_candidates_summary
        ON rh_entity_candidates (summary_id, created_at DESC);

    CREATE TABLE rh_entity_embeddings (
        entity_id UUID PRIMARY KEY REFERENCES rh_topics (id) ON DELETE CASCADE,
        embedding halfvec(1024) NOT NULL,
        embedded_at TIMESTAMPTZ NOT NULL,
        owner_user_id TEXT NOT NULL
    );
    CREATE INDEX idx_rh_entity_embeddings_hnsw
        ON rh_entity_embeddings USING hnsw (embedding halfvec_cosine_ops);

    FOREACH tbl IN ARRAY ARRAY['rh_entity_candidates', 'rh_entity_embeddings'] LOOP
        EXECUTE format('CREATE POLICY rh_read ON %I FOR SELECT USING (
            (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()))', tbl);
        EXECUTE format('CREATE POLICY rh_write ON %I USING (
            (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()))', tbl);
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', tbl);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', tbl);
    END LOOP;

    INSERT INTO _rivetos_migrations (name) VALUES ('0064_entity_match.sql');
END
$rivet$;
