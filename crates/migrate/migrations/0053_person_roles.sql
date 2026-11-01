DO $rivet$
DECLARE
    home text := current_schema();
    fn text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0053_person_roles.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_role_principals (
        role_name text PRIMARY KEY,
        user_id text NOT NULL CHECK (user_id <> '')
    );
    CREATE FUNCTION rh_role_principal_trigger() RETURNS trigger
        LANGUAGE plpgsql
        AS $$
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = NEW.role_name) THEN
                RAISE EXCEPTION 'rh_role_principals: % is not a role of this database server', NEW.role_name;
            END IF;
            RETURN NEW;
        END $$;
    CREATE TRIGGER rh_role_principal BEFORE INSERT OR UPDATE ON rh_role_principals
        FOR EACH ROW EXECUTE FUNCTION rh_role_principal_trigger();
    CREATE POLICY rh_read ON rh_role_principals FOR SELECT USING (true);
    ALTER TABLE rh_role_principals ENABLE ROW LEVEL SECURITY;
    GRANT SELECT ON rh_role_principals TO PUBLIC;

    EXECUTE format($f$
        CREATE FUNCTION rh_scope_pin() RETURNS text
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(
                      (SELECT user_id FROM rh_role_principals WHERE role_name = session_user::text),
                      (SELECT user_id FROM rh_role_principals WHERE role_name = current_user::text)) $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_principal() RETURNS text
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(rh_scope_pin(), nullif(current_setting('rh.principal', true), '')) $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_spaces() RETURNS text[]
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT CASE
                      WHEN rh_scope_pin() IS NULL
                      THEN coalesce(string_to_array(nullif(current_setting('rh.spaces', true), ''), ','), '{}'::text[])
                      ELSE coalesce((SELECT array_agg(space_id ORDER BY space_id) FROM rh_space_members
                                      WHERE user_id = rh_scope_pin()), '{}'::text[]) END $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_maintenance() RETURNS boolean
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(current_setting('rh.maintenance', true), '') = 'on'
                     AND NOT EXISTS (SELECT 1 FROM rh_role_principals WHERE role_name = current_user::text)
                     AND (NOT EXISTS (SELECT 1 FROM rh_role_principals WHERE role_name = session_user::text)
                          OR current_user::regrole = (SELECT p.proowner FROM pg_proc p
                                                       WHERE p.oid = 'rh_elevate'::regproc)) $$
        $f$, home);

    DROP POLICY rh_read ON rh_space_members;
    CREATE POLICY rh_read ON rh_space_members FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR user_id = (SELECT rh_scope_principal()));
    DROP POLICY rh_write ON rh_message_access;
    CREATE POLICY rh_write ON rh_message_access USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_messages m WHERE m.id = message_id));
    DROP POLICY rh_write ON rh_summary_access;
    CREATE POLICY rh_write ON rh_summary_access USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_summaries s WHERE s.id = summary_id));

    CREATE OR REPLACE FUNCTION rh_wiki_owner_trigger() RETURNS trigger
        LANGUAGE plpgsql
        AS $$
        DECLARE
            previous text := rh_elevate();
        BEGIN
            IF NEW.owner_user_id IS NULL THEN
                NEW.owner_user_id := coalesce(rh_scope_principal(),
                    (SELECT min(c.owner_user_id) FROM rh_conversations c HAVING count(DISTINCT c.owner_user_id) = 1));
            END IF;
            PERFORM rh_restore(previous);
            RETURN NEW;
        END $$;

    FOR fn IN SELECT p.oid::regprocedure::text FROM pg_proc p
               JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname = home AND p.proname <> 'rh_elevate'
                AND (p.prosrc LIKE '%rh_elevate()%' OR p.prosrc LIKE '%rh_refresh_%') LOOP
        EXECUTE format('ALTER FUNCTION %s SECURITY DEFINER SET search_path = pg_catalog, %I, pg_temp', fn, home);
    END LOOP;
    FOREACH fn IN ARRAY ARRAY['rh_refresh_sessions(text[])', 'rh_refresh_topics(uuid[])', 'rh_refresh_owner(text)'] LOOP
        EXECUTE format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', fn);
    END LOOP;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0053_person_roles.sql');
    END IF;
END
$rivet$;
