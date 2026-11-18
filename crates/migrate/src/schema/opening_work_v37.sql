-- Snapshot legacy sessions once. Live readers and writers use only openings.
WITH first_leaf AS (
    SELECT id, conversation_id,
           row_number() OVER (
               PARTITION BY conversation_id ORDER BY earliest_at IS NULL, earliest_at, created_at, id
           ) AS place
      FROM ros_summaries
     WHERE kind = 'leaf' AND conversation_id IS NOT NULL
       AND trim(content, char(32, 9, 13, 10)) <> ''
)
INSERT INTO rh_summary_openings (conversation_id, content, filing_armed, title_armed)
SELECT c.id, substr(trim(s.content, char(32, 9, 13, 10)), 1, 2000),
       NOT EXISTS (SELECT 1 FROM rh_conversation_placements p WHERE p.conversation_id = c.id)
       AND NOT EXISTS (
           SELECT 1 FROM rh_session_placements p
            WHERE p.session_id = c.session_key
               OR (c.channel = 'claude-code' AND p.session_id = 'claude-code:' || c.session_key)
       ),
       c.title IS NULL OR lower(trim(c.title)) IN (
           '', 'untitled', 'new session', 'new chat', 'new conversation',
           'claude code session', 'claude-code session', 'codex session',
           'grok build session', 'grok-build session', 'kimi code session', 'kimi-code session',
           'dsh session', 'opencode session', 'pi session', 'qwen code session', 'qwen-code session'
       )
  FROM first_leaf f
  JOIN ros_summaries s ON s.id = f.id
  JOIN ros_conversations c ON c.id = f.conversation_id
 WHERE f.place = 1 AND c.session_key NOT LIKE 'heartbeat:%'
ON CONFLICT (conversation_id) DO NOTHING;
