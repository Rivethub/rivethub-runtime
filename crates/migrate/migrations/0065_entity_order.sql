DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0065_entity_order.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_entity_applied (
        entity_id UUID PRIMARY KEY REFERENCES rh_topics (id) ON DELETE CASCADE,
        applied_through TIMESTAMPTZ NOT NULL,
        summary_id UUID NOT NULL,
        owner_user_id TEXT NOT NULL
    );

    CREATE POLICY rh_read ON rh_entity_applied FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_write ON rh_entity_applied USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_entity_applied ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_applied FORCE ROW LEVEL SECURITY;

    INSERT INTO _rivetos_migrations (name) VALUES ('0065_entity_order.sql');
END
$rivet$;
