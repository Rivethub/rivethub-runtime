DO $rivet$
DECLARE
    home text := current_schema();
    store_owner text := nullif(current_setting('rh.store_owner', true), '');
    owners bigint;
    tbl text;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0056_session_claims.sql') THEN
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

    FOREACH tbl IN ARRAY ARRAY['rh_session_startup', 'rh_session_suggestions', 'rh_session_keeps',
                               'rh_filing_corrections', 'rh_session_tombstones'] LOOP
        EXECUTE format('ALTER TABLE %I ADD COLUMN owner_user_id text', tbl);
        IF store_owner IS NULL THEN
            EXECUTE format('ALTER TABLE %I ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal()', tbl);
        ELSE
            EXECUTE format('ALTER TABLE %I ALTER COLUMN owner_user_id SET DEFAULT coalesce(rh_scope_principal(), %L)', tbl, store_owner);
        END IF;
    END LOOP;
    UPDATE rh_session_startup t SET owner_user_id = coalesce(rh_session_owner(t.session_id, NULL), store_owner);
    UPDATE rh_session_suggestions t SET owner_user_id = coalesce(rh_session_owner(t.session_id, NULL), store_owner);
    UPDATE rh_session_keeps t SET owner_user_id = coalesce(rh_session_owner(t.session_key, NULL), store_owner);
    UPDATE rh_filing_corrections t SET owner_user_id = coalesce(rh_session_owner(t.session_id, t.conversation_id), store_owner);
    UPDATE rh_session_tombstones t SET owner_user_id = coalesce(rh_session_owner(t.session_key, NULL), store_owner);

    FOREACH tbl IN ARRAY ARRAY['rh_session_startup', 'rh_session_suggestions', 'rh_session_keeps', 'rh_filing_corrections'] LOOP
        EXECUTE format('DROP POLICY rh_own ON %I', tbl);
        EXECUTE format('CREATE POLICY rh_own ON %I USING (
            (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()))', tbl);
    END LOOP;
    DROP POLICY rh_write ON rh_session_tombstones;
    CREATE POLICY rh_write ON rh_session_tombstones USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));

    EXECUTE format($f$
        CREATE FUNCTION rh_session_claim_trigger() RETURNS trigger
            LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, %1$I, pg_temp
            AS $$
            DECLARE
                previous text := rh_elevate();
                claimant text := coalesce(NEW.owner_user_id, rh_scope_principal());
                family text[] := ARRAY[NEW.session_key, 'claude-code:' || NEW.session_key,
                                       substring(NEW.session_key FROM '^claude-code:(.+)$')];
                holder text;
            BEGIN
                IF claimant IS NOT NULL THEN
                    SELECT c.owner_user_id INTO holder FROM rh_conversations c
                     WHERE c.session_key = ANY (family) AND c.owner_user_id IS DISTINCT FROM claimant
                       AND c.id::text IS DISTINCT FROM (to_jsonb(NEW) ->> 'id')
                       AND NOT (c.session_key = NEW.session_key AND c.agent IS NOT DISTINCT FROM (to_jsonb(NEW) ->> 'agent'))
                     LIMIT 1;
                END IF;
                PERFORM rh_restore(previous);
                IF holder IS NOT NULL THEN
                    RAISE EXCEPTION 'session %% belongs to another person', NEW.session_key
                        USING ERRCODE = 'insufficient_privilege';
                END IF;
                RETURN NEW;
            END $$
        $f$, home);
    REVOKE EXECUTE ON FUNCTION rh_session_claim_trigger() FROM PUBLIC;
    CREATE TRIGGER rh_conversation_claim BEFORE INSERT OR UPDATE OF session_key, owner_user_id ON rh_conversations
        FOR EACH ROW EXECUTE FUNCTION rh_session_claim_trigger();
    CREATE TRIGGER rh_tombstone_claim BEFORE INSERT OR UPDATE OF session_key, owner_user_id ON rh_session_tombstones
        FOR EACH ROW EXECUTE FUNCTION rh_session_claim_trigger();

    FOREACH tbl IN ARRAY ARRAY['rh_job_failures', 'rh_worker_runs'] LOOP
        EXECUTE format('CREATE POLICY rh_own ON %I USING (
            (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_pin()) IS NULL)', tbl);
    END LOOP;
    CREATE POLICY rh_read ON rh_topic_tombstones FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_topic_tombstones USING (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_pin()) IS NULL
        OR EXISTS (SELECT 1 FROM rh_topics t
                    WHERE t.kind = rh_topic_tombstones.kind AND t.slug = rh_topic_tombstones.slug
                      AND t.owner_user_id = (SELECT rh_scope_principal())));
    ALTER TABLE rh_tag_taxonomy ADD COLUMN owner_user_id text DEFAULT rh_scope_pin();
    CREATE POLICY rh_read ON rh_tag_taxonomy FOR SELECT USING (true);
    CREATE POLICY rh_write ON rh_tag_taxonomy USING (
        (SELECT rh_scope_maintenance()) OR (SELECT rh_scope_pin()) IS NULL
        OR owner_user_id = (SELECT rh_scope_principal()));
    FOREACH tbl IN ARRAY ARRAY['rh_job_failures', 'rh_worker_runs', 'rh_topic_tombstones', 'rh_tag_taxonomy'] LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', tbl);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', tbl);
    END LOOP;

    DROP POLICY rh_write ON rh_wiki_topics;
    CREATE POLICY rh_write ON rh_wiki_topics USING (
        (SELECT rh_scope_maintenance())
        OR ((owner_user_id = (SELECT rh_scope_principal())
             OR (visibility <> 'private' AND visibility = ANY ((SELECT rh_scope_spaces())::text[])))
            AND ((SELECT rh_scope_pin()) IS NULL
                 OR EXISTS (SELECT 1 FROM rh_wiki_scopes s
                             WHERE s.scope = rh_wiki_topics.scope AND s.visibility = rh_wiki_topics.visibility
                               AND rh_wiki_topics.slug = s.key_prefix || rh_wiki_topics.local_slug
                               AND rh_wiki_topics.owner_user_id
                                   = CASE WHEN s.visibility = 'private' THEN s.owner_user_id ELSE s.scope END))));

    CREATE OR REPLACE FUNCTION notify_chunk_embedding_queue() RETURNS trigger AS $func$
    BEGIN
        IF current_setting('rivet.defer_embed_enqueue', true) = 'on' THEN
            RETURN NEW;
        END IF;
        IF NEW.embedding IS NULL
           AND NEW.embed_status IS NULL
           AND NEW.content IS NOT NULL
           AND LENGTH(TRIM(NEW.content)) > 0 THEN
            PERFORM graphile_worker.add_job(
                'embed-target',
                json_build_object('targetTable', 'ros_message_chunks', 'targetId', NEW.id),
                job_key      := 'embed-ros_message_chunks-' || NEW.id::text,
                max_attempts := 5
            );
        END IF;
        RETURN NEW;
    END;
    $func$ LANGUAGE plpgsql;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0056_session_claims.sql');
    END IF;
END
$rivet$;
