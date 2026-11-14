DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0068_entity_page_binding.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    UPDATE rh_wiki_topics AS page
       SET topic_id = entity.id
      FROM rh_topics AS entity
     WHERE page.topic_id IS NULL
       AND entity.kind = 'entity'
       AND entity.slug = page.slug
       AND NOT EXISTS (
           SELECT 1 FROM rh_wiki_topics bound WHERE bound.topic_id = entity.id
       )
       AND NOT EXISTS (
           SELECT 1
             FROM rh_topics other
            WHERE other.slug = page.slug
              AND other.kind <> 'entity'
       )
       AND NOT EXISTS (
           SELECT 1
             FROM rh_topics twin
            WHERE twin.kind = 'entity'
              AND twin.slug = page.slug
              AND twin.id <> entity.id
       );

    INSERT INTO _rivetos_migrations (name) VALUES ('0068_entity_page_binding.sql');
END
$rivet$;
