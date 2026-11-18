UPDATE ros_summary_deliveries AS d
SET anchor_id = (SELECT a.anchor FROM summary_anchors a WHERE a.message = d.message_id),
    historical_backlog = NOT EXISTS (SELECT 1 FROM summary_anchors a WHERE a.message = d.message_id)
WHERE d.anchor_id IN (SELECT old FROM summary_replacements);

CREATE TEMP TABLE summary_replaced_access AS
SELECT map.new, sum(s.access_count) AS access_count, max(s.last_accessed_at) AS last_accessed_at
FROM ros_summaries s JOIN summary_replacements map ON map.old = s.id
WHERE map.new IS NOT NULL GROUP BY map.new;

CREATE TEMP TABLE summary_replaced_extractions AS
SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
       (e.status IN ('done', 'skipped')) DESC, e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
FROM ros_wiki_extractions e
JOIN summary_replacements map ON map.old = e.summary_id AND map.new IS NOT NULL;

DELETE FROM ros_summaries WHERE id IN (SELECT old FROM summary_replacements);
