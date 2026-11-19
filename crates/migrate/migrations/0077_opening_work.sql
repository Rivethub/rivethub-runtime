DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0077_opening_work.sql') THEN
        RETURN;
    END IF;

    -- Snapshot legacy sessions once. Live readers and writers use only openings.
    PERFORM set_config('rh.maintenance', 'on', true);
    WITH first_leaf AS (
        SELECT DISTINCT ON (conversation_id) conversation_id, id
          FROM rh_summaries
         WHERE kind = 'leaf' AND conversation_id IS NOT NULL
           AND btrim(content, E' \t\r\n') <> ''
         ORDER BY conversation_id, earliest_at NULLS LAST, created_at, id
    )
    INSERT INTO rh_summary_openings (conversation_id, content, historical_pending)
    SELECT c.id, left(btrim(s.content, E' \t\r\n'), 2000), true
      FROM first_leaf f
      JOIN rh_summaries s ON s.id = f.id
      JOIN rh_conversations c ON c.id = f.conversation_id
     WHERE c.session_key NOT LIKE 'heartbeat:%'
       AND NOT EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key)
    ON CONFLICT (conversation_id) DO NOTHING;

    INSERT INTO _rivetos_migrations (name) VALUES ('0077_opening_work.sql');
END
$rivet$;
