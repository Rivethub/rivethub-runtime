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
INSERT INTO rh_summary_openings (conversation_id, content, historical_pending)
SELECT c.id, substr(trim(s.content, char(32, 9, 13, 10)), 1, 2000), 1
  FROM first_leaf f
  JOIN ros_summaries s ON s.id = f.id
  JOIN ros_conversations c ON c.id = f.conversation_id
 WHERE f.place = 1 AND c.session_key NOT LIKE 'heartbeat:%'
   AND NOT EXISTS (SELECT 1 FROM rh_session_trash t WHERE t.session_key = c.session_key)
ON CONFLICT (conversation_id) DO NOTHING;

CREATE TRIGGER IF NOT EXISTS rh_disarm_trashed_openings
AFTER INSERT ON rh_session_trash
BEGIN
    UPDATE rh_summary_openings SET filing_armed = 0, title_armed = 0
    WHERE conversation_id IN (SELECT id FROM ros_conversations WHERE session_key = NEW.session_key)
      AND (filing_armed OR title_armed);
END;
