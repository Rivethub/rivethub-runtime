WITH ranked AS (
    SELECT e.*, row_number() OVER (
        PARTITION BY new ORDER BY (status = 'done') DESC, (status = 'skipped') DESC,
        pipeline_version DESC, rh_instant(extracted_at) DESC, summary_id DESC
    ) AS rank
    FROM summary_entity_extractions e
)
INSERT INTO rh_entity_extractions
    (summary_id, status, pipeline_version, entities_touched, git_sha, error, extracted_at, owner_user_id)
SELECT e.new, e.status, e.pipeline_version, (SELECT json_group_array(slug) FROM (
    SELECT DISTINCT item.value AS slug FROM summary_entity_extractions r, json_each(r.entities_touched) item
    WHERE r.new = e.new ORDER BY slug
)), e.git_sha, e.error, e.extracted_at, e.owner_user_id
FROM ranked e WHERE e.rank = 1
ON CONFLICT (summary_id) DO UPDATE
SET status = excluded.status, pipeline_version = excluded.pipeline_version,
    entities_touched = excluded.entities_touched, git_sha = excluded.git_sha,
    error = excluded.error, extracted_at = excluded.extracted_at;

WITH ranked AS (
    SELECT h.*, row_number() OVER (
        PARTITION BY entity_id, new ORDER BY rh_instant(source_at) DESC, summary_id DESC
    ) AS rank
    FROM summary_entity_history h
)
INSERT INTO rh_entity_history (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
SELECT entity_id, new, conversation_id, session_on, sentence, owner_user_id FROM ranked WHERE rank = 1
ON CONFLICT (entity_id, summary_id) DO UPDATE SET conversation_id = excluded.conversation_id,
    session_on = excluded.session_on, sentence = excluded.sentence;

WITH ranked AS (
    SELECT t.*, row_number() OVER (
        PARTITION BY entity_id, new, type_name ORDER BY rh_instant(observed_at) DESC, summary_id DESC
    ) AS rank
    FROM summary_entity_types t
)
INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
SELECT entity_id, new, type_name, observed_at, owner_user_id FROM ranked WHERE rank = 1
ON CONFLICT (entity_id, summary_id, type_name) DO UPDATE SET observed_at = excluded.observed_at;

UPDATE rh_entity_applied AS current SET summary_id = map.new
FROM summary_replacements map WHERE current.summary_id = map.old AND map.new IS NOT NULL;

CREATE TEMP TABLE summary_entity_removed_marks AS
SELECT c.entity_id, c.owner_user_id FROM rh_entity_applied c
JOIN summary_replacements map ON c.summary_id = map.old WHERE map.new IS NULL;

DELETE FROM rh_entity_applied WHERE entity_id IN (SELECT entity_id FROM summary_entity_removed_marks);

WITH ranked AS (
    SELECT h.entity_id, h.summary_id, h.owner_user_id, coalesce(s.latest_at, s.created_at) AS applied_at,
           row_number() OVER (
               PARTITION BY h.entity_id
               ORDER BY rh_instant(coalesce(s.latest_at, s.created_at)) DESC, h.summary_id DESC
           ) AS rank
    FROM summary_entity_removed_marks old
    JOIN rh_entity_history h ON h.entity_id = old.entity_id AND h.owner_user_id = old.owner_user_id
    JOIN ros_summaries s ON s.id = h.summary_id
)
INSERT INTO rh_entity_applied (entity_id, summary_id, applied_through, owner_user_id)
SELECT entity_id, summary_id, applied_at, owner_user_id FROM ranked WHERE rank = 1 ON CONFLICT DO NOTHING;

DROP TABLE summary_entity_removed_marks;
DROP TABLE summary_entity_types;
DROP TABLE summary_entity_history;
DROP TABLE summary_entity_extractions;
