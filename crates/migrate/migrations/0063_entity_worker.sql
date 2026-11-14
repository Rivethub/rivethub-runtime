DO $rivet$
DECLARE
    tbl text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0063_entity_worker.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
    SELECT gen_random_uuid(), 'type', seed.slug, seed.name, now(), now()
      FROM (VALUES ('organizations', 'Organizations'),
                   ('projects', 'Projects'),
                   ('places', 'Places')) AS seed (slug, name)
     WHERE NOT EXISTS (
         SELECT 1 FROM rh_topics t WHERE t.kind = 'type' AND t.slug = seed.slug
     );

    CREATE TABLE rh_entity_extractions (
        summary_id UUID PRIMARY KEY REFERENCES rh_summaries (id) ON DELETE CASCADE,
        status TEXT NOT NULL CHECK (status IN ('done', 'skipped', 'failed')),
        pipeline_version INTEGER NOT NULL,
        entities_touched TEXT[] NOT NULL DEFAULT '{}',
        git_sha TEXT,
        error TEXT,
        extracted_at TIMESTAMPTZ NOT NULL,
        owner_user_id TEXT NOT NULL
    );
    CREATE INDEX idx_rh_entity_extractions_status
        ON rh_entity_extractions (status, extracted_at DESC);

    CREATE TABLE rh_entity_history (
        entity_id UUID NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
        summary_id UUID NOT NULL REFERENCES rh_summaries (id) ON DELETE CASCADE,
        conversation_id UUID,
        session_on DATE NOT NULL,
        sentence TEXT NOT NULL CHECK (sentence <> ''),
        owner_user_id TEXT NOT NULL,
        PRIMARY KEY (entity_id, summary_id)
    );

    FOREACH tbl IN ARRAY ARRAY['rh_entity_extractions', 'rh_entity_history'] LOOP
        EXECUTE format('CREATE POLICY rh_read ON %I FOR SELECT USING (
            (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()))', tbl);
        EXECUTE format('CREATE POLICY rh_write ON %I USING (
            (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()))', tbl);
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', tbl);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', tbl);
    END LOOP;

    INSERT INTO _rivetos_migrations (name) VALUES ('0063_entity_worker.sql');
END
$rivet$;
