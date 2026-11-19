pub(super) const REBIND_DELIVERIES: &str = r"UPDATE ros_summary_deliveries AS d
SET anchor_id = (SELECT a.anchor FROM summary_anchors a WHERE a.message = d.message_id),
    historical_backlog = NOT EXISTS (SELECT 1 FROM summary_anchors a WHERE a.message = d.message_id)
WHERE d.conversation_id = ?1 AND d.anchor_id IN (SELECT old FROM summary_replacements)";

pub(super) const BEFORE_DELETE: &str = r"CREATE TEMP TABLE summary_reference_map AS
SELECT old, new, kind FROM summary_replacements WHERE new IS NOT NULL
UNION ALL
SELECT DISTINCT map.new, map.new, map.kind
FROM summary_replacements map JOIN summary_retained kept ON kept.id = map.new;

CREATE TEMP TABLE summary_replaced_access AS
SELECT map.new, sum(s.access_count) AS access_count, max(s.last_accessed_at) AS last_accessed_at
FROM ros_summaries s JOIN summary_reference_map map ON map.old = s.id
WHERE map.new IS NOT NULL GROUP BY map.new;

CREATE TEMP TABLE summary_replaced_extractions AS
SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
       (e.status IN ('done', 'skipped')) DESC, e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
FROM ros_wiki_extractions e
JOIN summary_reference_map map ON map.old = e.summary_id AND map.new IS NOT NULL;

DELETE FROM ros_summaries WHERE id IN (SELECT old FROM summary_replacements);
";

pub(super) const COVERAGE: &str = r"WITH RECURSIVE tree AS (
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
";

pub(super) const DELIVERIES: &str = r"WITH legacy_leaves AS (
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
";

pub(super) const REFERENCES: &str = r"UPDATE ros_summaries AS s
SET access_count = a.access_count, last_accessed_at = a.last_accessed_at
FROM summary_replaced_access a WHERE s.id = a.new;

INSERT INTO ros_wiki_extractions (summary_id, status, pipeline_version, topics_touched, git_sha, error, extracted_at)
SELECT e.new, e.status, e.pipeline_version,
       (SELECT json_group_array(topic) FROM (
           SELECT DISTINCT t.value AS topic
           FROM summary_replaced_extractions r, json_each(r.topics_touched) t
           WHERE r.new = e.new ORDER BY topic
        )), e.git_sha, e.error, e.extracted_at
FROM summary_replaced_extractions e WHERE e.rank = 1
ON CONFLICT (summary_id) DO UPDATE
SET status = excluded.status, pipeline_version = excluded.pipeline_version,
    topics_touched = excluded.topics_touched, git_sha = excluded.git_sha,
    error = excluded.error, extracted_at = excluded.extracted_at;

WITH ranked AS (
    SELECT c.*, map.new, map.kind AS new_kind,
        row_number() OVER (PARTITION BY c.topic_slug, map.new ORDER BY
            (nullif(trim(c.note), '') IS NOT NULL) DESC, c.cited_at DESC, map.old DESC) AS rank
    FROM ros_wiki_citations c JOIN summary_reference_map map ON map.old = c.summary_id AND map.new IS NOT NULL
)
INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
SELECT topic_slug, new, CASE WHEN kind IS NOT NULL THEN new_kind END, note, cited_at
FROM ranked WHERE rank = 1
ON CONFLICT (topic_slug, summary_id) DO UPDATE
SET kind = excluded.kind, note = excluded.note, cited_at = excluded.cited_at;

WITH ranked AS (
    SELECT p.*, map.new,
        first_value(p.conversation_id) OVER (PARTITION BY p.topic_slug, map.new ORDER BY
            (p.conversation_id IS NOT NULL) DESC, p.created_at, map.old) AS conversation,
        first_value(p.git_sha) OVER (PARTITION BY p.topic_slug, map.new ORDER BY
            (p.git_sha IS NOT NULL) DESC, p.created_at, map.old) AS sha
    FROM ros_wiki_provenance p
    JOIN summary_reference_map map ON map.old = p.source_id AND map.new IS NOT NULL
    WHERE p.source_kind = 'summary'
)
INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at)
SELECT topic_slug, 'summary', new, conversation, sha, min(created_at)
FROM ranked WHERE true GROUP BY topic_slug, new, conversation, sha
ON CONFLICT (topic_slug, source_kind, source_id) DO UPDATE
SET conversation_id = excluded.conversation_id, git_sha = excluded.git_sha, created_at = excluded.created_at;

CREATE TEMP TABLE summary_replaced_held AS
SELECT c.*, map.new,
       CASE WHEN c.source = c.summary_id THEN map.new
            WHEN c.source = 'summary:' || c.summary_id THEN 'summary:' || map.new
            ELSE c.source END AS new_source
FROM rh_wiki_entity_candidates c JOIN summary_reference_map map ON map.old = c.summary_id
WHERE map.new IS NOT NULL;

DELETE FROM rh_wiki_entity_candidates WHERE summary_id IN (SELECT old FROM summary_replacements);
WITH ranked AS (
    SELECT h.*, row_number() OVER (PARTITION BY h.key, h.new_source ORDER BY h.held_at DESC, h.summary_id DESC) AS rank
    FROM summary_replaced_held h
)
INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
SELECT key, new_source, new, patch, held_at, owner_user_id FROM ranked WHERE rank = 1
ON CONFLICT (key, source) DO UPDATE
SET summary_id = excluded.summary_id, patch = excluded.patch, held_at = excluded.held_at
WHERE rh_wiki_entity_candidates.owner_user_id = excluded.owner_user_id
  AND rh_wiki_entity_candidates.held_at <= excluded.held_at;

WITH ranked AS (
    SELECT d.*, map.new, row_number() OVER (
        PARTITION BY map.new ORDER BY d.deferred_at DESC, map.old DESC
    ) AS rank
    FROM rh_wiki_deferred d JOIN summary_reference_map map ON map.old = d.summary_id
)
INSERT INTO rh_wiki_deferred (summary_id, waits_session, waits_conversation, applied_topic, patches, deferred_at, owner_user_id)
SELECT d.new, d.waits_session, d.waits_conversation, d.applied_topic,
       (SELECT json_group_array(json(patch)) FROM (
           SELECT patch FROM (
               SELECT item.value AS patch, r.rank AS source_rank, item.key AS ordinal,
                      row_number() OVER (PARTITION BY json_extract(item.value, '$.slug')
                          ORDER BY r.rank, item.key) AS patch_rank
               FROM ranked r, json_each(r.patches) item
               WHERE r.new = d.new AND r.owner_user_id = d.owner_user_id
                 AND (r.waits_session, r.waits_conversation, r.applied_topic)
                     IS (d.waits_session, d.waits_conversation, d.applied_topic)
           ) WHERE patch_rank = 1 ORDER BY source_rank, ordinal
       )), d.deferred_at, d.owner_user_id
FROM ranked d WHERE d.rank = 1
ON CONFLICT (summary_id) DO UPDATE
SET waits_session = excluded.waits_session, waits_conversation = excluded.waits_conversation,
    applied_topic = excluded.applied_topic, patches = excluded.patches, deferred_at = excluded.deferred_at;

CREATE TEMP TABLE summary_claim_ranks AS
SELECT c.id, map.new, row_number() OVER (PARTITION BY c.fact_id, map.new ORDER BY
       c.at DESC, c.recorded_at DESC, c.id DESC) AS rank
FROM rh_entity_fact_claims c JOIN summary_reference_map map ON map.old = c.summary_id;

DELETE FROM rh_entity_fact_claims WHERE id IN (SELECT id FROM summary_claim_ranks WHERE rank > 1);
UPDATE rh_entity_fact_claims AS c SET summary_id = ranked.new
FROM summary_claim_ranks ranked WHERE c.id = ranked.id AND ranked.rank = 1 AND c.summary_id <> ranked.new;

CREATE TEMP TABLE summary_tag_ranks AS
SELECT t.id, map.new, row_number() OVER (PARTITION BY t.key, t.value, map.new ORDER BY
       (t.state IN ('accepted', 'rejected')) DESC, t.updated_at DESC, t.id DESC) AS rank
FROM ros_tags t JOIN summary_reference_map map ON map.old = t.entity_id
WHERE t.entity_type = 'summary';

DELETE FROM ros_tags WHERE id IN (SELECT id FROM summary_tag_ranks WHERE rank > 1);
UPDATE ros_tags AS t SET entity_id = ranked.new
FROM summary_tag_ranks ranked WHERE t.id = ranked.id AND ranked.rank = 1 AND t.entity_id <> ranked.new;

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

DROP TABLE summary_claim_ranks;
DROP TABLE summary_tag_ranks;
DROP TABLE summary_reference_map;
DROP TABLE summary_replaced_held;
DROP TABLE summary_replaced_access;
DROP TABLE summary_replaced_extractions;
DROP TABLE summary_anchors;
DROP TABLE summary_replacements;

DROP TABLE summary_retained;
";

pub(super) const REUSE_VECTORS: &str = r"CREATE TEMP TABLE summary_old_vectors AS
SELECT s.id, s.conversation_id, s.content, s.index_key, s.embedding
FROM ros_summaries s JOIN summary_replacements r ON r.old = s.id
WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
  AND length(s.embedding) = 4096 AND s.index_key IS NOT NULL;
";
