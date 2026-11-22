DO $rivet$
BEGIN
    SET LOCAL lock_timeout = '5s';
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0083_exact_entity_matches.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    DROP INDEX IF EXISTS idx_rh_entity_embeddings_hnsw;

    INSERT INTO _rivetos_migrations (name) VALUES ('0083_exact_entity_matches.sql');
END
$rivet$;
