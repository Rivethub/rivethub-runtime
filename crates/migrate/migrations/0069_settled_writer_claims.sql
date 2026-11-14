DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM _rivetos_migrations WHERE name = '0069_settled_writer_claims.sql'
        ) THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    ALTER TABLE rh_entity_fact_claims
        ADD COLUMN settled boolean NOT NULL DEFAULT false;
    UPDATE rh_entity_fact_claims
       SET settled = true
     WHERE actor = 'wiki-writer';

    INSERT INTO _rivetos_migrations (name) VALUES ('0069_settled_writer_claims.sql');
END
$rivet$;
