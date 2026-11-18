UPDATE ros_summaries AS s
SET access_count = a.access_count, last_accessed_at = a.last_accessed_at
FROM summary_replaced_access a WHERE s.id = a.new AND s.id NOT IN (SELECT id FROM summary_retained);

INSERT OR IGNORE INTO ros_wiki_extractions (summary_id, status, pipeline_version, topics_touched, git_sha, error, extracted_at)
SELECT e.new, e.status, e.pipeline_version,
       (SELECT json_group_array(topic) FROM (
           SELECT DISTINCT t.value AS topic
           FROM summary_replaced_extractions r, json_each(r.topics_touched) t
           WHERE r.new = e.new ORDER BY topic
        )), e.git_sha, e.error, e.extracted_at
FROM summary_replaced_extractions e WHERE e.rank = 1;

WITH ranked AS (
    SELECT c.*, map.new, map.kind AS new_kind,
        row_number() OVER (PARTITION BY c.topic_slug, map.new ORDER BY
            (nullif(trim(c.note), '') IS NOT NULL) DESC, c.cited_at DESC, map.old DESC) AS rank
    FROM ros_wiki_citations c JOIN summary_replacements map ON map.old = c.summary_id AND map.new IS NOT NULL
)
INSERT OR IGNORE INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
SELECT topic_slug, new, CASE WHEN kind IS NOT NULL THEN new_kind END, note, cited_at
FROM ranked WHERE rank = 1;

WITH ranked AS (
    SELECT p.*, map.new,
        first_value(p.conversation_id) OVER (PARTITION BY p.topic_slug, map.new ORDER BY
            (p.conversation_id IS NOT NULL) DESC, p.created_at, map.old) AS conversation,
        first_value(p.git_sha) OVER (PARTITION BY p.topic_slug, map.new ORDER BY
            (p.git_sha IS NOT NULL) DESC, p.created_at, map.old) AS sha
    FROM ros_wiki_provenance p
    JOIN summary_replacements map ON map.old = p.source_id AND map.new IS NOT NULL
    WHERE p.source_kind = 'summary'
)
INSERT OR IGNORE INTO ros_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at)
SELECT topic_slug, 'summary', new, conversation, sha, min(created_at)
FROM ranked GROUP BY topic_slug, new, conversation, sha;

CREATE TEMP TABLE summary_replaced_held AS
SELECT c.*, map.new,
       CASE WHEN c.source = c.summary_id THEN map.new
            WHEN c.source = 'summary:' || c.summary_id THEN 'summary:' || map.new
            ELSE c.source END AS new_source
FROM rh_wiki_entity_candidates c JOIN summary_replacements map ON map.old = c.summary_id
WHERE map.new IS NOT NULL;

DELETE FROM rh_wiki_entity_candidates WHERE summary_id IN (SELECT old FROM summary_replacements);
WITH ranked AS (
    SELECT h.*, row_number() OVER (PARTITION BY h.key, h.new_source ORDER BY h.held_at DESC, h.summary_id DESC) AS rank
    FROM summary_replaced_held h
)
INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
SELECT key, new_source, new, patch, held_at, owner_user_id FROM ranked WHERE rank = 1
ON CONFLICT (key, source) DO NOTHING;

WITH ranked AS (
    SELECT d.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY d.deferred_at DESC, map.old DESC) AS rank
    FROM rh_wiki_deferred d JOIN summary_replacements map ON map.old = d.summary_id AND map.new IS NOT NULL
)
INSERT OR IGNORE INTO rh_wiki_deferred (summary_id, waits_session, waits_conversation, applied_topic, patches, deferred_at, owner_user_id)
SELECT new, waits_session, waits_conversation, applied_topic, patches, deferred_at, owner_user_id
FROM ranked WHERE rank = 1;

WITH ranked AS (
    SELECT c.id, map.new, row_number() OVER (PARTITION BY c.fact_id, map.new ORDER BY
        c.at DESC, c.recorded_at DESC, c.id DESC) AS rank
    FROM rh_entity_fact_claims c
    JOIN summary_replacements map ON map.old = c.summary_id AND map.new IS NOT NULL
)
UPDATE rh_entity_fact_claims AS c SET summary_id = ranked.new
FROM ranked WHERE c.id = ranked.id AND ranked.rank = 1
AND NOT EXISTS (SELECT 1 FROM rh_entity_fact_claims existing
                WHERE existing.fact_id = c.fact_id AND existing.summary_id = ranked.new);

WITH ranked AS (
    SELECT t.id, map.new, row_number() OVER (PARTITION BY t.key, t.value, map.new ORDER BY
        t.updated_at DESC, t.id DESC) AS rank
    FROM ros_tags t JOIN summary_replacements map ON map.old = t.entity_id AND map.new IS NOT NULL
    WHERE t.entity_type = 'summary'
)
UPDATE ros_tags AS t SET entity_id = ranked.new FROM ranked WHERE t.id = ranked.id AND ranked.rank = 1
AND NOT EXISTS (SELECT 1 FROM ros_tags existing
                WHERE existing.entity_type = 'summary' AND existing.entity_id = ranked.new
                  AND existing.key = t.key AND existing.value = t.value);

DELETE FROM ros_tags WHERE entity_type = 'summary' AND entity_id IN (SELECT old FROM summary_replacements);
DELETE FROM rh_entity_fact_claims WHERE summary_id IN (SELECT old FROM summary_replacements);
DELETE FROM rh_wiki_deferred WHERE summary_id IN (SELECT old FROM summary_replacements);
DELETE FROM ros_wiki_citations WHERE summary_id IN (SELECT old FROM summary_replacements);
DELETE FROM ros_wiki_provenance WHERE source_kind = 'summary' AND source_id IN (SELECT old FROM summary_replacements);
DELETE FROM ros_jobs WHERE state <> 'running' AND json_valid(payload) AND (
    json_extract(payload, '$.summaryId') IN (SELECT old FROM summary_replacements)
    OR (json_extract(payload, '$.targetTable') = 'ros_summaries'
        AND json_extract(payload, '$.targetId') IN (SELECT old FROM summary_replacements))
);

DROP TABLE summary_replaced_held;
DROP TABLE summary_replaced_access;
DROP TABLE summary_replaced_extractions;
DROP TABLE summary_anchors;
DROP TABLE summary_replacements;

DROP TABLE summary_retained;
