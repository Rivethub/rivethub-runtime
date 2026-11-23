DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0081_worker_reports.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_worker_reports (
        node TEXT NOT NULL CHECK (node <> ''),
        role TEXT NOT NULL CHECK (role <> ''),
        provider TEXT,
        model TEXT,
        outage TEXT,
        index_key TEXT,
        reported_at TIMESTAMPTZ NOT NULL,
        PRIMARY KEY (node, role)
    );
    CREATE POLICY rh_read ON rh_worker_reports FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_worker_reports USING (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_pin()) IS NULL);
    ALTER TABLE rh_worker_reports ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_worker_reports FORCE ROW LEVEL SECURITY;

    INSERT INTO _rivetos_migrations (name) VALUES ('0081_worker_reports.sql');
END
$rivet$;
