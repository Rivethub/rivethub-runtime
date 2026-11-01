DO $rivet$
DECLARE
    home text := current_schema();
    fn text;
    pinned text;
    tbl text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0055_person_queue.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    FOR fn IN SELECT p.oid::regprocedure::text FROM pg_proc p
               JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname = home AND p.prosecdef LOOP
        EXECUTE format('REVOKE EXECUTE ON FUNCTION %s FROM PUBLIC', fn);
    END LOOP;

    IF to_regnamespace('graphile_worker') IS NOT NULL THEN
        FOR pinned IN SELECT role_name FROM rh_role_principals
                       WHERE EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) LOOP
            FOREACH tbl IN ARRAY ARRAY['_private_jobs', '_private_job_queues', '_private_tasks'] LOOP
                EXECUTE format('DROP POLICY IF EXISTS %I ON graphile_worker.%I', pinned, tbl);
            END LOOP;
            EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA graphile_worker FROM %I', pinned);
            EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA graphile_worker FROM %I', pinned);
            EXECUTE format('REVOKE ALL ON ALL FUNCTIONS IN SCHEMA graphile_worker FROM %I', pinned);
            EXECUTE format('REVOKE ALL ON SCHEMA graphile_worker FROM %I', pinned);
        END LOOP;
    END IF;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0055_person_queue.sql');
    END IF;
END
$rivet$;
