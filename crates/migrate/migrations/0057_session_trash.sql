DO $rivet$
DECLARE
    store_owner text := nullif(current_setting('rh.store_owner', true), '');
    owners bigint;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0057_session_trash.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    IF store_owner IS NULL THEN
        SELECT min(owner_user_id), count(DISTINCT owner_user_id) INTO store_owner, owners FROM rh_conversations;
        IF owners <> 1 THEN
            store_owner := NULL;
        END IF;
    END IF;

    CREATE TABLE rh_session_trash (
        session_key TEXT PRIMARY KEY CHECK (session_key <> ''),
        trashed_at TIMESTAMPTZ NOT NULL,
        trashed_by TEXT,
        owner_user_id TEXT
    );
    IF store_owner IS NULL THEN
        ALTER TABLE rh_session_trash ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal();
    ELSE
        EXECUTE format('ALTER TABLE rh_session_trash ALTER COLUMN owner_user_id SET DEFAULT coalesce(rh_scope_principal(), %L)', store_owner);
    END IF;
    CREATE INDEX rh_session_trash_trashed_at ON rh_session_trash (trashed_at, session_key);

    CREATE POLICY rh_own ON rh_session_trash USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_session_trash ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_session_trash FORCE ROW LEVEL SECURITY;
    CREATE TRIGGER rh_trash_claim BEFORE INSERT OR UPDATE OF session_key, owner_user_id ON rh_session_trash
        FOR EACH ROW EXECUTE FUNCTION rh_session_claim_trigger();

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0057_session_trash.sql');
    END IF;
END
$rivet$;
