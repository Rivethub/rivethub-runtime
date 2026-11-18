INSERT INTO rh_summary_access (summary_id, access_count, last_accessed_at)
SELECT map.new, least(2147483647, sum(coalesce(a.access_count, s.access_count, 0))),
       max(coalesce(a.last_accessed_at, s.last_accessed_at))
FROM summary_replacements map
JOIN rh_summaries s ON s.id = map.old
LEFT JOIN rh_summary_access a ON a.summary_id = map.old
WHERE map.new IS NOT NULL AND coalesce(a.last_accessed_at, s.last_accessed_at) IS NOT NULL
GROUP BY map.new
ON CONFLICT (summary_id) DO NOTHING;

UPDATE rh_summaries dest SET access_count = totals.count, last_accessed_at = totals.last_access
FROM (
    SELECT map.new, least(2147483647, sum(coalesce(a.access_count, s.access_count, 0))) AS count,
           max(coalesce(a.last_accessed_at, s.last_accessed_at)) AS last_access
    FROM summary_replacements map JOIN rh_summaries s ON s.id = map.old
    LEFT JOIN rh_summary_access a ON a.summary_id = map.old
    WHERE map.new IS NOT NULL GROUP BY map.new
) totals
WHERE dest.id = totals.new AND totals.last_access IS NULL AND totals.count > 0
  AND dest.id NOT IN (SELECT id FROM summary_retained);

INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
SELECT DISTINCT ON (c.topic_slug, map.new) c.topic_slug, map.new,
       CASE WHEN c.kind IS NOT NULL THEN map.kind END, c.note, c.cited_at, c.owner_user_id
FROM rh_wiki_citations c JOIN summary_replacements map ON map.old = c.summary_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = c.owner_user_id
ORDER BY c.topic_slug, map.new, (nullif(btrim(c.note), '') IS NOT NULL) DESC, c.cited_at DESC, map.old DESC
ON CONFLICT DO NOTHING;

INSERT INTO rh_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
SELECT p.topic_slug, 'summary', map.new,
       (array_agg(p.conversation_id ORDER BY (p.conversation_id IS NOT NULL) DESC, p.created_at, map.old))[1],
       (array_agg(p.git_sha ORDER BY (p.git_sha IS NOT NULL) DESC, p.created_at, map.old))[1],
       min(p.created_at), dest.owner_user_id
FROM rh_wiki_provenance p JOIN summary_replacements map ON map.old = p.source_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = p.owner_user_id
WHERE p.source_kind = 'summary'
GROUP BY p.topic_slug, map.new, dest.owner_user_id
ON CONFLICT DO NOTHING;

WITH ranked AS (
    SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
           (e.status IN ('done', 'skipped')) DESC, e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
    FROM rh_wiki_extractions e JOIN summary_replacements map ON map.old = e.summary_id AND map.new IS NOT NULL
    JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = e.owner_user_id
)
INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, topics_touched, git_sha, error, extracted_at, owner_user_id)
SELECT e.new, e.status, e.pipeline_version, ARRAY(
           SELECT DISTINCT topic FROM ranked r CROSS JOIN LATERAL unnest(r.topics_touched) topic
           WHERE r.new = e.new ORDER BY topic
       ), e.git_sha, e.error, e.extracted_at, e.owner_user_id
FROM ranked e WHERE e.rank = 1
ON CONFLICT DO NOTHING;

WITH ranked AS (
    SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
           (e.status = 'done') DESC, (e.status = 'skipped') DESC,
           e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
    FROM rh_entity_extractions e JOIN summary_replacements map ON map.old = e.summary_id AND map.new IS NOT NULL
    JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = e.owner_user_id
)
INSERT INTO rh_entity_extractions (summary_id, status, pipeline_version, entities_touched, git_sha, error, extracted_at, owner_user_id)
SELECT e.new, e.status, e.pipeline_version, ARRAY(
           SELECT DISTINCT entity FROM ranked r CROSS JOIN LATERAL unnest(r.entities_touched) entity
           WHERE r.new = e.new ORDER BY entity
       ), e.git_sha, e.error, e.extracted_at, e.owner_user_id
FROM ranked e WHERE e.rank = 1
ON CONFLICT DO NOTHING;

WITH removed AS (
    DELETE FROM rh_wiki_entity_candidates c USING summary_replacements map
    WHERE c.summary_id = map.old RETURNING c.*, map.new
), mapped AS (
    SELECT c.*,
           CASE WHEN c.source = c.summary_id::text THEN c.new::text
                WHEN c.source = 'summary:' || c.summary_id THEN 'summary:' || c.new
                ELSE c.source END AS new_source
    FROM removed c
    JOIN rh_summaries dest ON dest.id = c.new AND dest.owner_user_id = c.owner_user_id
)
INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
SELECT DISTINCT ON (key, new_source) key, new_source, new, patch, held_at, owner_user_id
FROM mapped ORDER BY key, new_source, held_at DESC, summary_id DESC
ON CONFLICT (key, source) DO NOTHING;

INSERT INTO rh_wiki_deferred (summary_id, waits_session, waits_conversation, applied_topic, patches, deferred_at, owner_user_id)
SELECT DISTINCT ON (map.new) map.new, d.waits_session, d.waits_conversation, d.applied_topic,
       d.patches, d.deferred_at, d.owner_user_id
FROM rh_wiki_deferred d JOIN summary_replacements map ON map.old = d.summary_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = d.owner_user_id
ORDER BY map.new, d.deferred_at DESC, map.old DESC
ON CONFLICT DO NOTHING;

INSERT INTO rh_entity_history (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
SELECT DISTINCT ON (h.entity_id, map.new) h.entity_id, map.new, h.conversation_id,
       h.session_on, h.sentence, h.owner_user_id
FROM rh_entity_history h JOIN summary_replacements map ON map.old = h.summary_id AND map.new IS NOT NULL
JOIN rh_summaries source ON source.id = map.old
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = h.owner_user_id
ORDER BY h.entity_id, map.new, coalesce(source.latest_at, source.created_at) DESC, map.old DESC
ON CONFLICT DO NOTHING;

INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
SELECT DISTINCT ON (s.entity_id, map.new, s.type_name) s.entity_id, map.new, s.type_name, s.observed_at, s.owner_user_id
FROM rh_entity_type_seen s JOIN summary_replacements map ON map.old = s.summary_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = s.owner_user_id
ORDER BY s.entity_id, map.new, s.type_name, s.observed_at DESC, map.old DESC
ON CONFLICT DO NOTHING;

UPDATE rh_entity_candidates c SET summary_id = map.new FROM summary_replacements map
JOIN rh_summaries dest ON dest.id = map.new
WHERE c.summary_id = map.old AND c.owner_user_id = dest.owner_user_id;

UPDATE rh_entity_decisions d SET summary_id = map.new FROM summary_replacements map
JOIN rh_summaries dest ON dest.id = map.new
WHERE d.summary_id = map.old AND d.owner_user_id = dest.owner_user_id;

UPDATE rh_entity_applied c SET summary_id = map.new FROM summary_replacements map
JOIN rh_summaries dest ON dest.id = map.new
WHERE c.summary_id = map.old AND c.owner_user_id = dest.owner_user_id;

WITH removed AS (
    DELETE FROM rh_entity_applied c USING summary_replacements map
    WHERE c.summary_id = map.old RETURNING c.entity_id, c.owner_user_id
)
INSERT INTO rh_entity_applied (entity_id, summary_id, applied_through, owner_user_id)
SELECT DISTINCT ON (h.entity_id) h.entity_id, h.summary_id,
       coalesce(s.latest_at, s.created_at), h.owner_user_id
FROM removed old JOIN rh_entity_history h ON h.entity_id = old.entity_id AND h.owner_user_id = old.owner_user_id
JOIN rh_summaries s ON s.id = h.summary_id
WHERE NOT EXISTS (SELECT 1 FROM summary_replacements map WHERE map.old = h.summary_id)
ORDER BY h.entity_id, coalesce(s.latest_at, s.created_at) DESC, h.summary_id DESC
ON CONFLICT DO NOTHING;

WITH ranked AS (
    SELECT c.id, map.new, row_number() OVER (PARTITION BY c.fact_id, map.new ORDER BY
        c.at DESC, c.recorded_at DESC, c.id DESC) AS rank
    FROM rh_entity_fact_claims c
    JOIN summary_replacements map ON map.old = c.summary_id AND map.new IS NOT NULL
    JOIN rh_summaries dest ON dest.id = map.new AND c.owner_user_id = dest.owner_user_id
)
UPDATE rh_entity_fact_claims c SET summary_id = ranked.new
FROM ranked WHERE c.id = ranked.id AND ranked.rank = 1
AND NOT EXISTS (SELECT 1 FROM rh_entity_fact_claims existing
                WHERE existing.fact_id = c.fact_id AND existing.summary_id = ranked.new);

WITH ranked AS (
    SELECT t.id, map.new, row_number() OVER (PARTITION BY t.key, t.value, map.new ORDER BY
        t.updated_at DESC, t.id DESC) AS rank
    FROM rh_tags t JOIN summary_replacements map ON map.old = t.entity_id AND map.new IS NOT NULL
    WHERE t.entity_type = 'summary'
)
UPDATE rh_tags t SET entity_id = ranked.new FROM ranked WHERE t.id = ranked.id AND ranked.rank = 1
AND NOT EXISTS (SELECT 1 FROM rh_tags existing
                WHERE existing.entity_type = 'summary' AND existing.entity_id = ranked.new
                  AND existing.key = t.key AND existing.value = t.value);

DELETE FROM rh_tags t USING summary_replacements map WHERE t.entity_type = 'summary' AND t.entity_id = map.old;
DELETE FROM rh_entity_fact_claims c USING summary_replacements map WHERE c.summary_id = map.old;
DELETE FROM rh_wiki_deferred d USING summary_replacements map WHERE d.summary_id = map.old;
DELETE FROM rh_wiki_citations c USING summary_replacements map WHERE c.summary_id = map.old;
DELETE FROM rh_wiki_provenance p USING summary_replacements map WHERE p.source_kind = 'summary' AND p.source_id = map.old;
DELETE FROM rh_summaries s USING summary_replacements map WHERE s.id = map.old;
