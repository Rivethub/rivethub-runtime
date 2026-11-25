DO $rivet$
DECLARE
    cascade_key name;
BEGIN
    SET LOCAL lock_timeout = '10s';
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM _rivetos_migrations WHERE name = '0086_wiki_redirect_writes.sql'
        ) THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    LOCK TABLE rh_wiki_topics IN SHARE ROW EXCLUSIVE MODE;
    LOCK TABLE rh_wiki_redirects IN ACCESS EXCLUSIVE MODE;

    FOR cascade_key IN
        SELECT conname FROM pg_constraint
         WHERE conrelid = 'rh_wiki_redirects'::regclass AND contype = 'f'
           AND confrelid = 'rh_wiki_topics'::regclass
    LOOP
        EXECUTE format('ALTER TABLE rh_wiki_redirects DROP CONSTRAINT %I', cascade_key);
    END LOOP;
    ALTER TABLE rh_wiki_redirects ADD CONSTRAINT rh_wiki_redirects_to_slug_fkey
        FOREIGN KEY (to_slug) REFERENCES rh_wiki_topics (slug) ON DELETE RESTRICT;

    UPDATE rh_wiki_redirects r SET owner_user_id = w.owner_user_id
      FROM rh_wiki_topics w
     WHERE w.slug = r.to_slug AND r.owner_user_id IS NULL
       AND w.visibility = 'private' AND w.owner_user_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM rh_spaces s WHERE s.id = w.owner_user_id);

    DROP POLICY rh_own ON rh_wiki_redirects;
    CREATE POLICY rh_read ON rh_wiki_redirects FOR SELECT USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_wiki_topics w
             WHERE w.slug = rh_wiki_redirects.to_slug
        ));
    CREATE POLICY rh_insert ON rh_wiki_redirects FOR INSERT WITH CHECK (
        (SELECT rh_scope_maintenance())
        OR (owner_user_id = (SELECT rh_scope_principal())
            AND EXISTS (
                SELECT 1 FROM rh_wiki_topics w
                 WHERE w.slug = rh_wiki_redirects.to_slug
            )));
    CREATE POLICY rh_update ON rh_wiki_redirects FOR UPDATE USING (
        (SELECT rh_scope_maintenance())
        OR (owner_user_id = (SELECT rh_scope_principal())
            AND EXISTS (
                SELECT 1 FROM rh_wiki_topics w
                 WHERE w.slug = rh_wiki_redirects.to_slug
            )))
    WITH CHECK (
        (SELECT rh_scope_maintenance())
        OR (owner_user_id = (SELECT rh_scope_principal())
            AND EXISTS (
                SELECT 1 FROM rh_wiki_topics w
                 WHERE w.slug = rh_wiki_redirects.to_slug
            )));
    CREATE POLICY rh_delete ON rh_wiki_redirects FOR DELETE USING (
        (SELECT rh_scope_maintenance())
        OR (owner_user_id = (SELECT rh_scope_principal())
            AND EXISTS (
                SELECT 1 FROM rh_wiki_topics w
                 WHERE w.slug = rh_wiki_redirects.to_slug
            )));

    INSERT INTO _rivetos_migrations (name) VALUES ('0086_wiki_redirect_writes.sql');
END
$rivet$;
