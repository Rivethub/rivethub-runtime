DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0061_session_keep_claim.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TRIGGER rh_keep_claim BEFORE INSERT OR UPDATE OF session_key, owner_user_id ON rh_session_keeps
        FOR EACH ROW EXECUTE FUNCTION rh_session_claim_trigger();

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0061_session_keep_claim.sql');
    END IF;
END
$rivet$;
