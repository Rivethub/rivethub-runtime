DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0066_entity_known_keys.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE INDEX idx_rh_topics_entity_name_key
        ON rh_topics (regexp_replace(lower(name), '[^a-z0-9]+', '', 'g'))
        WHERE kind = 'entity';

    CREATE INDEX idx_rh_topic_aliases_entity_key
        ON rh_topic_aliases (replace(lower(alias_key), '-', ''))
        WHERE kind = 'entity';

    INSERT INTO _rivetos_migrations (name) VALUES ('0066_entity_known_keys.sql');
END
$rivet$;
