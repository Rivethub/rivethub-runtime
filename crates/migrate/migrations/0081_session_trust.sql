DO $rivet$
DECLARE
    home text := current_schema();
    trusted text;
    tbl text;
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0081_session_trust.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    FOREACH trusted IN ARRAY ARRAY['rh_gateway', 'rh_maintenance'] LOOP
        IF to_regrole(trusted) IS NULL THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN', trusted);
            EXCEPTION
                WHEN duplicate_object OR unique_violation THEN
                    NULL;
                WHEN insufficient_privilege THEN
                    RAISE EXCEPTION 'role % does not exist and this login cannot create it', trusted
                        USING HINT = format('As a role administrator run CREATE ROLE %I NOLOGIN, then migrate again.', trusted);
            END;
        END IF;
        EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', home, trusted);
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA %I TO %I', home, trusted);
        EXECUTE format('GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA %I TO %I', home, trusted);
        EXECUTE format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA %I TO %I', home, trusted);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', home, trusted);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO %I', home, trusted);
        EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA %I GRANT EXECUTE ON FUNCTIONS TO %I', home, trusted);
        FOREACH tbl IN ARRAY ARRAY['rh_role_principals', '_rivetos_migrations', '_sqlx_migrations'] LOOP
            IF to_regclass(tbl) IS NOT NULL THEN
                EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON %I FROM %I', tbl, trusted);
            END IF;
        END LOOP;
        IF to_regnamespace('graphile_worker') IS NOT NULL THEN
            EXECUTE format('GRANT USAGE ON SCHEMA graphile_worker TO %I', trusted);
            EXECUTE format('GRANT ALL ON ALL TABLES IN SCHEMA graphile_worker TO %I', trusted);
            EXECUTE format('GRANT ALL ON ALL SEQUENCES IN SCHEMA graphile_worker TO %I', trusted);
            EXECUTE format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA graphile_worker TO %I', trusted);
            FOR tbl IN SELECT c.relname FROM pg_class c
                        WHERE c.relnamespace = 'graphile_worker'::regnamespace
                          AND c.relkind = 'r' AND c.relrowsecurity LOOP
                EXECUTE format('CREATE POLICY %I ON graphile_worker.%I TO %I USING (true)', trusted, tbl, trusted);
            END LOOP;
        END IF;
    END LOOP;

    EXECUTE format($f$
        CREATE FUNCTION rh_scope_asserts() RETURNS boolean
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(pg_has_role(session_user, to_regrole('rh_gateway'), 'MEMBER'), false)
                      OR pg_has_role(session_user, (SELECT p.proowner FROM pg_proc p
                                                     WHERE p.oid = 'rh_elevate'::regproc), 'MEMBER') $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_principal() RETURNS text
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(rh_scope_pin(),
                      CASE WHEN rh_scope_asserts() THEN nullif(current_setting('rh.principal', true), '') END) $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_spaces() RETURNS text[]
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT CASE
                      WHEN rh_scope_pin() IS NOT NULL
                      THEN coalesce((SELECT array_agg(space_id ORDER BY space_id) FROM rh_space_members
                                      WHERE user_id = rh_scope_pin()), '{}'::text[])
                      WHEN rh_scope_asserts()
                      THEN coalesce(string_to_array(nullif(current_setting('rh.spaces', true), ''), ','), '{}'::text[])
                      ELSE '{}'::text[] END $$
        $f$, home);
    EXECUTE format($f$
        CREATE OR REPLACE FUNCTION rh_scope_maintenance() RETURNS boolean
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(current_setting('rh.maintenance', true), '') = 'on'
                     AND NOT EXISTS (SELECT 1 FROM rh_role_principals WHERE role_name = current_user::text)
                     AND (current_user::regrole = (SELECT p.proowner FROM pg_proc p
                                                    WHERE p.oid = 'rh_elevate'::regproc)
                          OR (NOT EXISTS (SELECT 1 FROM rh_role_principals WHERE role_name = session_user::text)
                              AND coalesce(pg_has_role(current_user, to_regrole('rh_maintenance'), 'MEMBER'), false))) $$
        $f$, home);

    INSERT INTO _rivetos_migrations (name) VALUES ('0081_session_trust.sql');
END
$rivet$;
