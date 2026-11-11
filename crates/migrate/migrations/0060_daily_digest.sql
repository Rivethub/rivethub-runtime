DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0060_daily_digest.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_daily_digests (
        owner_user_id text NOT NULL CHECK (owner_user_id <> ''),
        day date NOT NULL,
        summary text NOT NULL CHECK (summary <> ''),
        changes integer NOT NULL CHECK (changes >= 0),
        written_at timestamptz NOT NULL,
        PRIMARY KEY (owner_user_id, day)
    );
    CREATE POLICY rh_own ON rh_daily_digests USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_daily_digests ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_daily_digests FORCE ROW LEVEL SECURITY;

    ALTER TABLE rh_topic_redirects ADD COLUMN merged_from text CHECK (merged_from <> '');
    CREATE INDEX rh_topic_redirects_merged ON rh_topic_redirects (created_at) WHERE merged_from IS NOT NULL;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0060_daily_digest.sql');
    END IF;
END
$rivet$;
