DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (
            SELECT 1 FROM _rivetos_migrations WHERE name = '0085_rls_write_hardening.sql'
        ) THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    DROP POLICY rh_write ON rh_topic_aliases;
    CREATE POLICY rh_write ON rh_topic_aliases USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_topics t
             WHERE t.id = topic_id
               AND t.owner_user_id = (SELECT rh_scope_principal())
        ));

    DROP POLICY rh_write ON rh_topic_decisions;
    CREATE POLICY rh_write ON rh_topic_decisions USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_topics t
             WHERE t.id = topic_id
               AND t.owner_user_id = (SELECT rh_scope_principal())
        ));

    DROP POLICY rh_write ON rh_entity_facts;
    CREATE POLICY rh_write ON rh_entity_facts USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_topics t
             WHERE t.id = topic_id
               AND t.owner_user_id = (SELECT rh_scope_principal())
        ));

    DROP POLICY rh_own ON rh_topic_redirects;
    CREATE POLICY rh_read ON rh_topic_redirects FOR SELECT USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_topics t
             WHERE t.id = rh_topic_redirects.topic_id
        ));
    CREATE POLICY rh_write ON rh_topic_redirects USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_topics t
             WHERE t.id = rh_topic_redirects.topic_id
               AND t.owner_user_id = (SELECT rh_scope_principal())
        ));

    DROP POLICY rh_own ON rh_summary_sources;
    CREATE POLICY rh_read ON rh_summary_sources FOR SELECT USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_summaries s
             WHERE s.id = rh_summary_sources.summary_id
        ));
    CREATE POLICY rh_write ON rh_summary_sources USING (
        (SELECT rh_scope_maintenance())
        OR EXISTS (
            SELECT 1 FROM rh_summaries s
             WHERE s.id = rh_summary_sources.summary_id
               AND s.owner_user_id = (SELECT rh_scope_principal())
        ));

    INSERT INTO _rivetos_migrations (name) VALUES ('0085_rls_write_hardening.sql');
END
$rivet$;
