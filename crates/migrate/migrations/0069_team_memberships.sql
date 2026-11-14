DO $rivet$
DECLARE
    home text := current_schema();
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0069_team_memberships.sql') THEN
            RETURN;
        END IF;
    END IF;
    EXECUTE format($f$
        CREATE FUNCTION rh_member_spaces() RETURNS text[]
            LANGUAGE plpgsql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, %1$I, pg_temp
            AS $$
            DECLARE
                person text := rh_scope_principal();
                previous text := coalesce(current_setting('rh.maintenance', true), '');
                spaces text[];
            BEGIN
                PERFORM set_config('rh.maintenance', 'on', true);
                SELECT coalesce(array_agg(space_id ORDER BY space_id), '{}'::text[])
                  INTO spaces FROM rh_space_members WHERE user_id = person;
                PERFORM set_config('rh.maintenance', previous, true);
                RETURN spaces;
            END $$
        $f$, home);
    GRANT EXECUTE ON FUNCTION rh_member_spaces() TO PUBLIC;
    DROP POLICY rh_read ON rh_space_members;
    CREATE POLICY rh_read ON rh_space_members FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR space_id = ANY ((SELECT rh_member_spaces())::text[]));
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0069_team_memberships.sql');
    END IF;
END
$rivet$;
