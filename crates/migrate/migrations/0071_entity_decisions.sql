DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM _rivetos_migrations WHERE name = '0071_entity_decisions.sql'
        ) THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_entity_decisions (
        id UUID PRIMARY KEY,
        summary_id UUID NOT NULL REFERENCES rh_summaries (id) ON DELETE CASCADE,
        kind TEXT NOT NULL CHECK (kind IN ('entity', 'type')),
        proposed_name TEXT NOT NULL,
        proposed_type TEXT NOT NULL,
        candidates JSONB NOT NULL,
        verdict TEXT NOT NULL CHECK (verdict IN ('same', 'different', 'none', 'unreachable', 'hold')),
        choice TEXT,
        confidence REAL,
        model TEXT NOT NULL,
        entity_id UUID,
        created_at TIMESTAMPTZ NOT NULL,
        owner_user_id TEXT NOT NULL
    );
    CREATE INDEX idx_rh_entity_decisions_summary
        ON rh_entity_decisions (summary_id, created_at DESC);

    CREATE POLICY rh_read ON rh_entity_decisions FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_write ON rh_entity_decisions USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_entity_decisions ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_decisions FORCE ROW LEVEL SECURITY;

    INSERT INTO _rivetos_migrations (name) VALUES ('0071_entity_decisions.sql');
END
$rivet$;
