DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0067_entity_name_identity.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_entity_type_seen (
        entity_id UUID NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
        summary_id UUID NOT NULL REFERENCES rh_summaries (id) ON DELETE CASCADE,
        type_name TEXT NOT NULL CHECK (type_name <> ''),
        observed_at TIMESTAMPTZ NOT NULL,
        owner_user_id TEXT NOT NULL,
        PRIMARY KEY (entity_id, summary_id, type_name)
    );
    CREATE INDEX idx_rh_entity_type_seen_recent
        ON rh_entity_type_seen (entity_id, observed_at DESC, summary_id DESC);

    CREATE POLICY rh_read ON rh_entity_type_seen FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_write ON rh_entity_type_seen USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_entity_type_seen ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_type_seen FORCE ROW LEVEL SECURITY;

    INSERT INTO _rivetos_migrations (name) VALUES ('0067_entity_name_identity.sql');
END
$rivet$;