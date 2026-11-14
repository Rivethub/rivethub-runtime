DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0074_membership_authority.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_membership_authority (
        singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
        node text NOT NULL CHECK (node <> ''),
        claimed_at timestamptz NOT NULL
    );
    CREATE POLICY rh_read ON rh_membership_authority FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_membership_authority USING ((SELECT rh_scope_maintenance()));
    ALTER TABLE rh_membership_authority ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_membership_authority FORCE ROW LEVEL SECURITY;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0074_membership_authority.sql');
    END IF;
END
$rivet$;
