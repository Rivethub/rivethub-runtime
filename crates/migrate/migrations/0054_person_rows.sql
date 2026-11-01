DO $rivet$
DECLARE
    home text := current_schema();
    store_owner text := nullif(current_setting('rh.store_owner', true), '');
    owners bigint;
    tbl text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0054_person_rows.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    IF store_owner IS NULL THEN
        SELECT min(owner_user_id), count(DISTINCT owner_user_id) INTO store_owner, owners FROM rh_conversations;
        IF owners <> 1 THEN
            store_owner := NULL;
        END IF;
    END IF;

    EXECUTE format($f$
        CREATE FUNCTION rh_session_owner(session text, conversation uuid) RETURNS text
            LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, %1$I, pg_temp
            AS $$ SELECT coalesce(
                      (SELECT c.owner_user_id FROM rh_conversations c WHERE c.id = conversation),
                      (SELECT c.owner_user_id FROM rh_conversations c
                        WHERE c.session_key IN (session, substring(session FROM '^claude-code:(.+)$'))
                        ORDER BY c.created_at, c.id LIMIT 1),
                      (SELECT p.owner_user_id FROM rh_session_placements p WHERE p.session_id = session)) $$
        $f$, home);

    ALTER TABLE rh_session_history ADD COLUMN owner_user_id text;
    UPDATE rh_session_history h SET owner_user_id = coalesce(rh_session_owner(h.session_id, NULL), store_owner);
    ALTER TABLE rh_agent_presets ADD COLUMN owner_user_id text;
    UPDATE rh_agent_presets SET owner_user_id = store_owner;
    IF EXISTS (SELECT 1 FROM rh_agent_presets WHERE owner_user_id IS NULL) THEN
        RAISE EXCEPTION 'migration 0054 gives each agent preset an owner and cannot tell whose the existing presets are, because this store holds conversations of more than one owner. Run it once with rh.store_owner naming that person (for example a connection URL ending in ?options=-c%%20rh.store_owner%%3D<user>).';
    END IF;
    IF store_owner IS NULL THEN
        ALTER TABLE rh_session_history ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal();
        ALTER TABLE rh_agent_presets ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal();
    ELSE
        EXECUTE format('ALTER TABLE rh_session_history ALTER COLUMN owner_user_id SET DEFAULT coalesce(rh_scope_principal(), %L)', store_owner);
        EXECUTE format('ALTER TABLE rh_agent_presets ALTER COLUMN owner_user_id SET DEFAULT coalesce(rh_scope_principal(), %L)', store_owner);
    END IF;
    ALTER TABLE rh_session_suggestions
        DROP CONSTRAINT rh_session_suggestions_topic_id_fkey,
        ADD CONSTRAINT rh_session_suggestions_topic_id_fkey
            FOREIGN KEY (topic_id) REFERENCES rh_topics (id) ON DELETE CASCADE;

    CREATE POLICY rh_own ON rh_session_startup USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_id, NULL::uuid) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_own ON rh_session_suggestions USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_id, NULL::uuid) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_own ON rh_session_keeps USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_key, NULL::uuid) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_own ON rh_filing_corrections USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_id, conversation_id) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_own ON rh_session_history USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_read ON rh_session_tombstones FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_session_tombstones USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_key, NULL::uuid) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_own ON rh_hub_state USING (
        (SELECT rh_scope_maintenance()) OR user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_read ON rh_agent_presets FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_principal()) IS NOT NULL);
    CREATE POLICY rh_write ON rh_agent_presets USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    CREATE POLICY rh_read ON rh_tasks_archive FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR rh_session_owner(session_key, conversation_id) = (SELECT rh_scope_principal()));
    CREATE POLICY rh_write ON rh_tasks_archive USING ((SELECT rh_scope_maintenance()));
    CREATE POLICY rh_own ON rh_delegation_runs_archive USING ((SELECT rh_scope_maintenance()));
    CREATE POLICY rh_own ON rh_message_counts_pending USING ((SELECT rh_scope_maintenance()));
    CREATE POLICY rh_own ON rh_flagged_jobs USING (
        (SELECT rh_scope_maintenance())
        OR ((SELECT rh_scope_pin()) IS NULL AND (SELECT rh_scope_principal()) IS NOT NULL));
    CREATE POLICY rh_flag ON rh_flagged_jobs FOR INSERT WITH CHECK (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_principal()) IS NOT NULL);
    CREATE POLICY rh_own ON rh_summary_sources USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_summaries s WHERE s.id = rh_summary_sources.summary_id));
    CREATE POLICY rh_own ON rh_topic_redirects USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_topics t WHERE t.id = rh_topic_redirects.topic_id));
    CREATE POLICY rh_own ON rh_wiki_redirects USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_wiki_topics w WHERE w.slug = rh_wiki_redirects.to_slug));
    FOREACH tbl IN ARRAY ARRAY['rh_backfills', '_rivetos_migrations', '_sqlx_migrations'] LOOP
        CONTINUE WHEN to_regclass(tbl) IS NULL;
        EXECUTE format('CREATE POLICY rh_read ON %I FOR SELECT USING (true)', tbl);
        EXECUTE format('CREATE POLICY rh_write ON %I USING ((SELECT rh_scope_maintenance()))', tbl);
    END LOOP;

    FOREACH tbl IN ARRAY ARRAY[
        'rh_session_startup', 'rh_session_suggestions', 'rh_session_keeps', 'rh_filing_corrections',
        'rh_session_history', 'rh_session_tombstones', 'rh_hub_state', 'rh_agent_presets',
        'rh_tasks_archive', 'rh_delegation_runs_archive', 'rh_message_counts_pending', 'rh_flagged_jobs',
        'rh_summary_sources', 'rh_topic_redirects', 'rh_wiki_redirects',
        'rh_backfills', '_rivetos_migrations', '_sqlx_migrations'] LOOP
        CONTINUE WHEN to_regclass(tbl) IS NULL;
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', tbl);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', tbl);
    END LOOP;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0054_person_rows.sql');
    END IF;
END
$rivet$;
