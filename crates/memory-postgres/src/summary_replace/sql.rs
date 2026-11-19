pub(super) const DELIVERIES: &str = r"WITH legacy_leaves AS (
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
";

pub(super) const REFERENCES: &str = r"CREATE TEMP TABLE summary_reference_map ON COMMIT DROP AS
SELECT old, new, kind FROM summary_replacements WHERE new IS NOT NULL
UNION ALL
SELECT DISTINCT map.new, map.new, map.kind
FROM summary_replacements map JOIN summary_retained kept ON kept.id = map.new;

WITH totals AS (
    SELECT map.new, least(2147483647, sum(coalesce(a.access_count, s.access_count, 0))) AS count,
           max(coalesce(a.last_accessed_at, s.last_accessed_at)) AS last_access
    FROM summary_reference_map map JOIN rh_summaries s ON s.id = map.old
    LEFT JOIN rh_summary_access a ON a.summary_id = map.old
    GROUP BY map.new
), stored AS (
    INSERT INTO rh_summary_access (summary_id, access_count, last_accessed_at)
    SELECT new, count, last_access FROM totals WHERE last_access IS NOT NULL
    ON CONFLICT (summary_id) DO UPDATE
    SET access_count = excluded.access_count, last_accessed_at = excluded.last_accessed_at
)
UPDATE rh_summaries dest SET access_count = totals.count, last_accessed_at = totals.last_access
FROM totals WHERE dest.id = totals.new
  AND (dest.access_count, dest.last_accessed_at) IS DISTINCT FROM (totals.count, totals.last_access)
  AND (totals.last_access IS NOT NULL OR totals.count > 0);

INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
SELECT DISTINCT ON (c.topic_slug, map.new) c.topic_slug, map.new,
       CASE WHEN c.kind IS NOT NULL THEN map.kind END, c.note, c.cited_at, c.owner_user_id
FROM rh_wiki_citations c JOIN summary_reference_map map ON map.old = c.summary_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = c.owner_user_id
ORDER BY c.topic_slug, map.new, (nullif(btrim(c.note), '') IS NOT NULL) DESC, c.cited_at DESC, map.old DESC
ON CONFLICT (topic_slug, summary_id) DO UPDATE SET kind = excluded.kind, note = excluded.note, cited_at = excluded.cited_at;

INSERT INTO rh_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
SELECT p.topic_slug, 'summary', map.new,
       (array_agg(p.conversation_id ORDER BY (p.conversation_id IS NOT NULL) DESC, p.created_at, map.old))[1],
       (array_agg(p.git_sha ORDER BY (p.git_sha IS NOT NULL) DESC, p.created_at, map.old))[1],
       min(p.created_at), dest.owner_user_id
FROM rh_wiki_provenance p JOIN summary_reference_map map ON map.old = p.source_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = p.owner_user_id
WHERE p.source_kind = 'summary'
GROUP BY p.topic_slug, map.new, dest.owner_user_id
ON CONFLICT (topic_slug, source_kind, source_id) DO UPDATE SET conversation_id = excluded.conversation_id, git_sha = excluded.git_sha, created_at = excluded.created_at;

WITH ranked AS (
    SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
           (e.status IN ('done', 'skipped')) DESC, e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
    FROM rh_wiki_extractions e JOIN summary_reference_map map ON map.old = e.summary_id AND map.new IS NOT NULL
    JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = e.owner_user_id
)
INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, topics_touched, git_sha, error, extracted_at, owner_user_id)
SELECT e.new, e.status, e.pipeline_version, ARRAY(
           SELECT DISTINCT topic FROM ranked r CROSS JOIN LATERAL unnest(r.topics_touched) topic
           WHERE r.new = e.new ORDER BY topic
       ), e.git_sha, e.error, e.extracted_at, e.owner_user_id
FROM ranked e WHERE e.rank = 1
ON CONFLICT (summary_id) DO UPDATE SET status = excluded.status, pipeline_version = excluded.pipeline_version,
    topics_touched = excluded.topics_touched, git_sha = excluded.git_sha, error = excluded.error,
    extracted_at = excluded.extracted_at;

WITH ranked AS (
    SELECT e.*, map.new, row_number() OVER (PARTITION BY map.new ORDER BY
           (e.status = 'done') DESC, (e.status = 'skipped') DESC,
           e.pipeline_version DESC, e.extracted_at DESC, map.old DESC) AS rank
    FROM rh_entity_extractions e JOIN summary_reference_map map ON map.old = e.summary_id AND map.new IS NOT NULL
    JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = e.owner_user_id
)
INSERT INTO rh_entity_extractions (summary_id, status, pipeline_version, entities_touched, git_sha, error, extracted_at, owner_user_id)
SELECT e.new, e.status, e.pipeline_version, ARRAY(
           SELECT DISTINCT entity FROM ranked r CROSS JOIN LATERAL unnest(r.entities_touched) entity
           WHERE r.new = e.new ORDER BY entity
       ), e.git_sha, e.error, e.extracted_at, e.owner_user_id
FROM ranked e WHERE e.rank = 1
ON CONFLICT (summary_id) DO UPDATE SET status = excluded.status, pipeline_version = excluded.pipeline_version,
    entities_touched = excluded.entities_touched, git_sha = excluded.git_sha, error = excluded.error,
    extracted_at = excluded.extracted_at;

CREATE TEMP TABLE summary_replaced_held ON COMMIT DROP AS
SELECT c.*, map.new,
       CASE WHEN c.source = c.summary_id::text THEN map.new::text
            WHEN c.source = 'summary:' || c.summary_id THEN 'summary:' || map.new
            ELSE c.source END AS new_source
FROM rh_wiki_entity_candidates c JOIN summary_reference_map map ON map.old = c.summary_id
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = c.owner_user_id;

DELETE FROM rh_wiki_entity_candidates c USING summary_replacements map WHERE c.summary_id = map.old;
INSERT INTO rh_wiki_entity_candidates (key, source, summary_id, patch, held_at, owner_user_id)
SELECT DISTINCT ON (key, new_source) key, new_source, new, patch, held_at, owner_user_id
FROM summary_replaced_held ORDER BY key, new_source, held_at DESC, summary_id DESC
ON CONFLICT (key, source) DO UPDATE
SET summary_id = excluded.summary_id, patch = excluded.patch, held_at = excluded.held_at
WHERE rh_wiki_entity_candidates.owner_user_id = excluded.owner_user_id
  AND rh_wiki_entity_candidates.held_at <= excluded.held_at;

WITH ranked AS (
    SELECT d.*, map.new, row_number() OVER (
        PARTITION BY map.new ORDER BY d.deferred_at DESC, map.old DESC
    ) AS rank
    FROM rh_wiki_deferred d JOIN summary_reference_map map ON map.old = d.summary_id
    JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = d.owner_user_id
)
INSERT INTO rh_wiki_deferred (summary_id, waits_session, waits_conversation, applied_topic, patches, deferred_at, owner_user_id)
SELECT d.new, d.waits_session, d.waits_conversation, d.applied_topic,
       coalesce((
           SELECT jsonb_agg(chosen.patch ORDER BY chosen.rank, chosen.ordinal)
           FROM (
               SELECT DISTINCT ON (item.patch->>'slug') item.patch, r.rank, item.ordinal
               FROM ranked r CROSS JOIN LATERAL jsonb_array_elements(r.patches)
                    WITH ORDINALITY item(patch, ordinal)
               WHERE r.new = d.new AND (r.waits_session, r.waits_conversation, r.applied_topic)
                     IS NOT DISTINCT FROM (d.waits_session, d.waits_conversation, d.applied_topic)
               ORDER BY item.patch->>'slug', r.rank, item.ordinal
           ) chosen
       ), '[]'::jsonb), d.deferred_at, d.owner_user_id
FROM ranked d WHERE d.rank = 1
ON CONFLICT (summary_id) DO UPDATE
SET waits_session = excluded.waits_session, waits_conversation = excluded.waits_conversation,
    applied_topic = excluded.applied_topic, patches = excluded.patches, deferred_at = excluded.deferred_at;

INSERT INTO rh_entity_history (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
SELECT DISTINCT ON (h.entity_id, map.new) h.entity_id, map.new, h.conversation_id,
       h.session_on, h.sentence, h.owner_user_id
FROM rh_entity_history h JOIN summary_reference_map map ON map.old = h.summary_id AND map.new IS NOT NULL
JOIN rh_summaries source ON source.id = map.old
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = h.owner_user_id
ORDER BY h.entity_id, map.new, coalesce(source.latest_at, source.created_at) DESC, map.old DESC
ON CONFLICT (entity_id, summary_id) DO UPDATE SET conversation_id = excluded.conversation_id, session_on = excluded.session_on,
    sentence = excluded.sentence;

INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
SELECT DISTINCT ON (s.entity_id, map.new, s.type_name) s.entity_id, map.new, s.type_name, s.observed_at, s.owner_user_id
FROM rh_entity_type_seen s JOIN summary_reference_map map ON map.old = s.summary_id AND map.new IS NOT NULL
JOIN rh_summaries dest ON dest.id = map.new AND dest.owner_user_id = s.owner_user_id
ORDER BY s.entity_id, map.new, s.type_name, s.observed_at DESC, map.old DESC
ON CONFLICT (entity_id, summary_id, type_name) DO UPDATE SET observed_at = excluded.observed_at;

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

CREATE TEMP TABLE summary_claim_ranks ON COMMIT DROP AS
SELECT c.id, map.new, row_number() OVER (PARTITION BY c.fact_id, map.new ORDER BY
       c.at DESC, c.recorded_at DESC, c.id DESC) AS rank
FROM rh_entity_fact_claims c JOIN summary_reference_map map ON map.old = c.summary_id
JOIN rh_summaries dest ON dest.id = map.new AND c.owner_user_id = dest.owner_user_id;

DELETE FROM rh_entity_fact_claims WHERE id IN (SELECT id FROM summary_claim_ranks WHERE rank > 1);
UPDATE rh_entity_fact_claims AS c SET summary_id = ranked.new
FROM summary_claim_ranks ranked WHERE c.id = ranked.id AND ranked.rank = 1 AND c.summary_id <> ranked.new;

CREATE TEMP TABLE summary_tag_ranks ON COMMIT DROP AS
SELECT t.id, map.new, row_number() OVER (PARTITION BY t.key, t.value, map.new ORDER BY
       t.updated_at DESC, t.id DESC) AS rank
FROM rh_tags t JOIN summary_reference_map map ON map.old = t.entity_id
WHERE t.entity_type = 'summary';

DELETE FROM rh_tags WHERE id IN (SELECT id FROM summary_tag_ranks WHERE rank > 1);
UPDATE rh_tags AS t SET entity_id = ranked.new
FROM summary_tag_ranks ranked WHERE t.id = ranked.id AND ranked.rank = 1 AND t.entity_id <> ranked.new;

DELETE FROM rh_tags t USING summary_replacements map WHERE t.entity_type = 'summary' AND t.entity_id = map.old;
DELETE FROM rh_entity_fact_claims c USING summary_replacements map WHERE c.summary_id = map.old;
DELETE FROM rh_wiki_deferred d USING summary_replacements map WHERE d.summary_id = map.old;
DELETE FROM rh_wiki_citations c USING summary_replacements map WHERE c.summary_id = map.old;
DELETE FROM rh_wiki_provenance p USING summary_replacements map WHERE p.source_kind = 'summary' AND p.source_id = map.old;
DELETE FROM rh_summaries s USING summary_replacements map WHERE s.id = map.old;
";

pub(super) const REUSE_PASSAGES: &str = r"WITH dest AS (
    SELECT * FROM rh_summaries WHERE id = $1
), identical AS (
    SELECT old.record_id
    FROM summary_old_passages old CROSS JOIN dest
    WHERE cardinality($2::integer[]) = 0 AND old.complete
      AND old.owner_content = dest.content AND old.index_key = dest.index_key
    ORDER BY (old.kind = 'session' AND dest.kind = 'session') DESC, old.record_id
    LIMIT 1
), windows AS (
    SELECT ordinality::integer - 1 AS ordinal, first AS char_start, last AS char_end
    FROM unnest($2::integer[], $3::integer[]) WITH ORDINALITY AS w(first, last, ordinality)
    UNION ALL
    SELECT old.ordinal, old.char_start, old.char_end
    FROM summary_old_passages old JOIN identical ON identical.record_id = old.record_id
), draft AS (
    SELECT w.*, dest.id AS record_id, dest.index_key, dest.kind, dest.earliest_at, dest.latest_at,
           substring(dest.content FROM w.char_start + 1 FOR w.char_end - w.char_start) AS content,
           coalesce(
               (SELECT array_agg(l.message_id ORDER BY l.ordinal) FROM rh_summary_sources l
                WHERE l.summary_id = dest.id AND EXISTS (
                    SELECT 1 FROM rh_summaries s WHERE s.parent_id = dest.id AND s.kind = 'section'
                      AND w.char_start < s.section_char_end AND w.char_end > s.section_char_start
                      AND l.ordinal BETWEEN s.source_first AND s.source_last)),
               ARRAY(SELECT l.message_id FROM rh_summary_sources l
                     WHERE l.summary_id = dest.id ORDER BY l.ordinal)
           ) AS messages
    FROM windows w CROSS JOIN dest
), chosen AS (
    SELECT draft.*, old.id AS old_id, old.embedding, old.same_slot
    FROM draft LEFT JOIN LATERAL (
        SELECT p.id, p.embedding,
               draft.kind = 'session' AND p.kind = 'session'
                   AND draft.ordinal = p.ordinal AND draft.messages = p.messages AS same_slot
        FROM summary_old_passages p
        WHERE p.index_key = draft.index_key AND p.content = draft.content
        ORDER BY same_slot DESC, p.id LIMIT 1
    ) old ON true
), prepared AS (
    SELECT chosen.*,
           coalesce(times.first, chosen.earliest_at) AS first,
           coalesce(times.last, chosen.latest_at) AS last
    FROM chosen LEFT JOIN LATERAL (
        SELECT min(m.created_at) AS first, max(m.created_at) AS last
        FROM rh_messages m WHERE m.id = ANY(chosen.messages)
    ) times ON true
), moved AS (
    UPDATE rh_summary_passages p
    SET record_id = $1, ordinal = new.ordinal, char_start = new.char_start, char_end = new.char_end,
        earliest_at = new.first, latest_at = new.last
    FROM prepared new WHERE new.same_slot AND p.id = new.old_id
)
INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end, content,
    earliest_at, latest_at, index_key, embedding)
SELECT gen_random_uuid(), $1, ordinal, char_start, char_end, content, first, last, index_key, embedding
FROM prepared WHERE same_slot IS NOT TRUE
";

pub(super) const SNAPSHOT_PASSAGES: &str = r"CREATE TEMP TABLE summary_old_passages ON COMMIT DROP AS
SELECT p.id, p.record_id, p.ordinal, p.char_start, p.char_end, p.content, p.index_key, p.embedding,
       owner.kind, owner.content AS owner_content, NOT owner.index_needs_repair
         AND owner.index_key = p.index_key AND rh_summary_record_indexed(owner.id) AS complete,
       coalesce(
           (SELECT array_agg(l.message_id ORDER BY l.ordinal) FROM rh_summary_sources l
            WHERE l.summary_id = owner.id AND EXISTS (
                SELECT 1 FROM rh_summaries s WHERE s.parent_id = owner.id AND s.kind = 'section'
                  AND p.char_start < s.section_char_end AND p.char_end > s.section_char_start
                  AND l.ordinal BETWEEN s.source_first AND s.source_last)),
           ARRAY(SELECT l.message_id FROM rh_summary_sources l
                 WHERE l.summary_id = owner.id ORDER BY l.ordinal)
       ) AS messages
FROM rh_summary_passages p JOIN rh_summaries owner ON owner.id = p.record_id
WHERE p.record_id = ANY($1) AND p.index_key = ANY($2) AND p.embedding IS NOT NULL
  AND p.content = substring(owner.content FROM p.char_start + 1 FOR p.char_end - p.char_start)
";
