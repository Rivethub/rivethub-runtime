DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0070_shared_session_state.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    DROP POLICY rh_own ON rh_session_trash;
    CREATE POLICY rh_read ON rh_session_trash FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal())
        OR EXISTS (SELECT 1 FROM rh_conversations c WHERE c.session_key = rh_session_trash.session_key));
    CREATE POLICY rh_write ON rh_session_trash USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));

    DELETE FROM rh_session_marks m
     WHERE EXISTS (SELECT 1 FROM rh_conversations c
                    WHERE c.session_key IN (m.session_key, 'claude-code:' || m.session_key,
                                            substring(m.session_key FROM '^claude-code:(.+)$'))
                      AND c.owner_user_id IS DISTINCT FROM m.owner_user_id);
    CREATE TRIGGER rh_mark_claim BEFORE INSERT OR UPDATE OF session_key, owner_user_id ON rh_session_marks
        FOR EACH ROW EXECUTE FUNCTION rh_session_claim_trigger();

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0070_shared_session_state.sql');
    END IF;
END
$rivet$;
