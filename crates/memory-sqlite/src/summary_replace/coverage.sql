WITH RECURSIVE tree AS (
    SELECT id, parent_id, kind, source_first, source_last, range_verified
    FROM ros_summaries WHERE conversation_id = ?1
), descendants AS (
    SELECT id AS old, id AS node FROM tree WHERE kind IN ('branch', 'root')
    UNION
    SELECT d.old, t.id FROM descendants d JOIN tree t ON t.parent_id = d.node
), membership AS (
    SELECT old, node AS source, NULL AS first, NULL AS last FROM descendants
    UNION
    SELECT id, id, NULL, NULL FROM tree WHERE kind IN ('leaf', 'session', 'part')
    UNION
    SELECT t.id, t.parent_id, t.source_first, t.source_last FROM tree t
    JOIN tree p ON p.id = t.parent_id AND p.kind IN ('part', 'session')
    WHERE t.kind = 'section' AND t.range_verified
      AND EXISTS (SELECT 1 FROM ros_summary_sources l WHERE l.summary_id = p.id AND l.ordinal = t.source_first)
      AND EXISTS (SELECT 1 FROM ros_summary_sources l WHERE l.summary_id = p.id AND l.ordinal = t.source_last)
)
SELECT DISTINCT map.old, l.message_id FROM membership map
JOIN ros_summary_sources l ON l.summary_id = map.source
JOIN ros_messages m ON m.id = l.message_id AND m.conversation_id = ?1
WHERE map.first IS NULL OR l.ordinal BETWEEN map.first AND map.last
