CREATE TEMP TABLE summary_old_vectors AS
SELECT s.id, s.conversation_id, s.content, s.index_key, s.embedding
FROM ros_summaries s JOIN summary_replacements r ON r.old = s.id
WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
  AND s.embedding IS NOT NULL AND s.index_key IS NOT NULL;
