WITH legacy_leaves AS (
    SELECT s.* FROM rh_summaries s WHERE s.conversation_id = $1 AND s.kind = 'leaf' AND s.owner_user_id = $3
), window_members AS (
    SELECT s.id AS summary_id, m.id AS message_id,
           count(*) OVER (PARTITION BY s.id) AS window_count, s.message_count
    FROM legacy_leaves s JOIN rh_messages m ON m.conversation_id = s.conversation_id AND m.owner_user_id = $3
         AND m.created_at BETWEEN s.earliest_at AND s.latest_at
    WHERE NOT EXISTS (SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = s.id)
), legacy_members AS (
    SELECT l.summary_id, l.message_id FROM rh_summary_sources l JOIN legacy_leaves s ON s.id = l.summary_id
    UNION ALL
    SELECT summary_id, message_id FROM window_members WHERE window_count = message_count
), accepted AS (
    SELECT l.message_id, 'wiki' AS consumer, max(coalesce(e.pipeline_version, 4)) AS version
    FROM legacy_leaves s JOIN legacy_members l ON l.summary_id = s.id
    LEFT JOIN rh_wiki_extractions e ON e.summary_id = s.id
    LEFT JOIN rh_wiki_deferred d ON d.summary_id = s.id AND d.owner_user_id = s.owner_user_id
    WHERE s.conversation_id = $1 AND s.owner_user_id = $3 AND s.kind = 'leaf'
      AND (d.summary_id IS NOT NULL OR (e.owner_user_id = s.owner_user_id AND
           (e.status = 'skipped' OR (e.status = 'done' AND
            (cardinality(e.topics_touched) = 0 OR e.git_sha IS NOT NULL)))))
    GROUP BY l.message_id
    UNION ALL
    SELECT l.message_id, 'entity', max(e.pipeline_version)
    FROM legacy_leaves s JOIN legacy_members l ON l.summary_id = s.id
    JOIN rh_entity_extractions e ON e.summary_id = s.id AND e.owner_user_id = s.owner_user_id
    WHERE s.conversation_id = $1 AND s.owner_user_id = $3 AND s.kind = 'leaf' AND e.status = 'done'
    GROUP BY l.message_id
), recovery AS (
    SELECT DISTINCT l.message_id, 'wiki' AS consumer
    FROM legacy_members l
    JOIN rh_wiki_extractions e ON e.summary_id = l.summary_id WHERE e.status = 'failed'
      AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'wiki-ext-' || l.summary_id)
    UNION
    SELECT DISTINCT l.message_id, 'entity'
    FROM legacy_members l
    JOIN rh_entity_extractions e ON e.summary_id = l.summary_id WHERE e.status = 'failed'
      AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'entity-ext-' || l.summary_id)
    UNION
    SELECT DISTINCT l.message_id, j.consumer FROM legacy_members l
    JOIN summary_recovery j ON j.summary_id = l.summary_id
), delivery AS (
    SELECT m.id AS message_id, c.consumer, done.version,
           CASE WHEN done.version IS NULL AND (NOT $2 OR recovery.message_id IS NOT NULL)
                THEN a.anchor END AS anchor_id
    FROM rh_messages m CROSS JOIN (VALUES ('wiki'), ('entity')) c(consumer)
    LEFT JOIN summary_anchors a ON a.message = m.id
    LEFT JOIN accepted done ON done.message_id = m.id AND done.consumer = c.consumer
    LEFT JOIN recovery ON recovery.message_id = m.id AND recovery.consumer = c.consumer
    WHERE m.conversation_id = $1 AND m.owner_user_id = $3 AND ($2 OR a.anchor IS NOT NULL)
)
INSERT INTO rh_summary_deliveries (conversation_id, consumer, message_id, completed_version, anchor_id, historical_backlog)
SELECT $1, consumer, message_id, version, anchor_id, version IS NULL AND anchor_id IS NULL FROM delivery
ON CONFLICT (conversation_id, consumer, message_id) DO UPDATE
SET completed_version = EXCLUDED.completed_version, anchor_id = NULL, historical_backlog = false
WHERE rh_summary_deliveries.completed_version IS NULL AND EXCLUDED.completed_version IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM summary_retained WHERE id = rh_summary_deliveries.anchor_id)
