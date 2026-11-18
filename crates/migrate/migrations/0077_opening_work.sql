DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0077_opening_work.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_conversations ADD COLUMN title_from_capture boolean NOT NULL DEFAULT false;

    -- Snapshot legacy sessions once. Live readers and writers use only openings.
    PERFORM set_config('rh.maintenance', 'on', true);
    WITH first_leaf AS (
        SELECT DISTINCT ON (conversation_id) conversation_id, id
          FROM rh_summaries
         WHERE kind = 'leaf' AND conversation_id IS NOT NULL
           AND btrim(content, E' \t\r\n') <> ''
         ORDER BY conversation_id, earliest_at NULLS LAST, created_at, id
    )
    INSERT INTO rh_summary_openings (conversation_id, content, filing_armed, title_armed)
    SELECT c.id, left(btrim(s.content, E' \t\r\n'), 2000),
           NOT EXISTS (SELECT 1 FROM rh_conversation_placements p WHERE p.conversation_id = c.id)
           AND NOT EXISTS (
               SELECT 1 FROM rh_session_placements p
                WHERE p.session_id = c.session_key
                   OR (c.channel = 'claude-code' AND p.session_id = 'claude-code:' || c.session_key)
           ),
           c.title IS NULL OR lower(btrim(c.title)) IN ('', 'untitled', 'new session', 'new chat', 'new conversation')
              OR btrim(c.title) ~* '^(claude[- ]code|codex|grok[- ]build|kimi[- ]code|dsh|opencode|pi|qwen[- ]code) session$'
      FROM first_leaf f
      JOIN rh_summaries s ON s.id = f.id
      JOIN rh_conversations c ON c.id = f.conversation_id
     WHERE c.session_key NOT LIKE 'heartbeat:%'
    ON CONFLICT (conversation_id) DO NOTHING;

    INSERT INTO _rivetos_migrations (name) VALUES ('0077_opening_work.sql');
END
$rivet$;
