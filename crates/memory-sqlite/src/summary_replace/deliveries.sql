WITH legacy_leaves AS (
    SELECT s.* FROM ros_summaries s WHERE s.conversation_id = ?1 AND s.kind = 'leaf'
), window_members AS (
    SELECT s.id AS summary_id, m.id AS message_id,
           count(*) OVER (PARTITION BY s.id) AS window_count, s.message_count
    FROM legacy_leaves s JOIN ros_messages m ON m.conversation_id = s.conversation_id
         AND m.created_at BETWEEN s.earliest_at AND s.latest_at
    WHERE NOT EXISTS (SELECT 1 FROM ros_summary_sources l WHERE l.summary_id = s.id)
), legacy_members AS (
    SELECT l.summary_id, l.message_id FROM ros_summary_sources l JOIN legacy_leaves s ON s.id = l.summary_id
    UNION ALL
    SELECT summary_id, message_id FROM window_members WHERE window_count = message_count
), accepted AS (
    SELECT l.message_id, max(coalesce(e.pipeline_version, 4)) AS version
    FROM legacy_leaves s JOIN legacy_members l ON l.summary_id = s.id
    LEFT JOIN ros_wiki_extractions e ON e.summary_id = s.id
    LEFT JOIN rh_wiki_deferred d ON d.summary_id = s.id
    WHERE s.conversation_id = ?1 AND s.kind = 'leaf'
      AND (d.summary_id IS NOT NULL OR e.status IN ('done', 'skipped'))
    GROUP BY l.message_id
), recovery AS (
    SELECT DISTINCT l.message_id
    FROM legacy_members l
    JOIN ros_wiki_extractions e ON e.summary_id = l.summary_id WHERE e.status = 'failed'
    UNION
    SELECT DISTINCT l.message_id
    FROM legacy_members l
    JOIN ros_jobs j ON j.job_key = 'wiki-ext-' || l.summary_id AND j.state <> 'dead'
), delivery AS (
    SELECT m.id AS message_id, c.consumer, done.version,
           CASE WHEN done.version IS NULL AND (NOT ?2 OR recovery.message_id IS NOT NULL)
                THEN a.anchor END AS anchor_id
    FROM ros_messages m CROSS JOIN (SELECT 'wiki' AS consumer UNION ALL SELECT 'entity') c
    LEFT JOIN summary_anchors a ON a.message = m.id
    LEFT JOIN accepted done ON done.message_id = m.id AND c.consumer = 'wiki'
    LEFT JOIN recovery ON recovery.message_id = m.id AND c.consumer = 'wiki'
    WHERE m.conversation_id = ?1 AND (?2 OR a.anchor IS NOT NULL)
)
INSERT INTO ros_summary_deliveries (conversation_id, consumer, message_id, completed_version, anchor_id, historical_backlog)
SELECT ?1, consumer, message_id, version, anchor_id, version IS NULL AND anchor_id IS NULL FROM delivery WHERE true
ON CONFLICT (conversation_id, consumer, message_id) DO UPDATE
SET completed_version = excluded.completed_version, anchor_id = NULL, historical_backlog = 0
WHERE ros_summary_deliveries.completed_version IS NULL AND excluded.completed_version IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM summary_retained WHERE id = ros_summary_deliveries.anchor_id)
