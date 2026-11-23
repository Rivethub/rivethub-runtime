DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0082_worker_pauses.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_worker_pauses (
        role TEXT PRIMARY KEY CHECK (role IN ('categorizer', 'wiki-builder', 'entity-builder')),
        paused_at TIMESTAMPTZ NOT NULL,
        paused_by TEXT
    );
    CREATE POLICY rh_read ON rh_worker_pauses FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_worker_pauses USING (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_pin()) IS NULL);
    ALTER TABLE rh_worker_pauses ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_worker_pauses FORCE ROW LEVEL SECURITY;

    INSERT INTO _rivetos_migrations (name) VALUES ('0082_worker_pauses.sql');
END
$rivet$;
