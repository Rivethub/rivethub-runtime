pub(super) const SOURCE_LINKS: &str = "\
SELECT summary_id, message_id
  FROM rh_summary_sources
 WHERE summary_id = ANY($1)";

pub(super) const WINDOW_MESSAGES: &str = "\
SELECT id FROM rh_messages
 WHERE conversation_id = $1
   AND created_at >= $2
   AND created_at <= $3";

pub(super) const TRANSCRIPT_MESSAGES: &str = "\
SELECT id, created_at, role, tool_name, content, tool_args, tool_result
  FROM rh_messages
 WHERE id = ANY($1)
 ORDER BY created_at, id";

pub(super) const LEAF: &str = "\
SELECT s.id, s.conversation_id, s.content,
       coalesce(s.earliest_at, s.created_at) AS start_at,
       coalesce(s.latest_at, s.earliest_at, s.created_at) AS end_at,
       s.owner_user_id
  FROM rh_summaries s WHERE s.id = $1";

pub(super) const DONE: &str = "\
SELECT entities_touched FROM rh_entity_extractions
 WHERE summary_id = $1 AND status = 'done' AND pipeline_version >= $2";

pub(super) const FOLLOWING: &str = "\
SELECT s.id, s.conversation_id, s.content,
       coalesce(s.earliest_at, s.created_at) AS start_at,
       coalesce(s.latest_at, s.earliest_at, s.created_at) AS end_at,
       s.owner_user_id,
       coalesce(e.status = 'done' AND e.pipeline_version >= $4, false) AS done,
       job.id AS job_id
  FROM rh_summaries s
  LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
  LEFT JOIN LATERAL (
      SELECT j.id FROM graphile_worker._private_jobs j
      JOIN graphile_worker._private_tasks t ON t.id = j.task_id
      WHERE j.key = 'entity-ext-' || s.id::text AND t.identifier = 'extract-entities'
      LIMIT 1
  ) job ON true
 WHERE s.conversation_id = $1 AND s.kind = 'leaf'
   AND (coalesce(s.earliest_at, s.created_at), s.id) > ($2, $3)
 ORDER BY coalesce(s.earliest_at, s.created_at), s.id
 LIMIT $5";

pub(super) const IDENTITY_HITS: &str = "\
WITH ents AS (
  SELECT t.id, t.parent_id, t.created_at, coalesce(ty.name, '') AS type_name,
         regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g') AS raw_name,
         regexp_replace(lower(t.slug), '[^a-z0-9]+', '', 'g') AS raw_slug,
         CASE WHEN strpos(t.name, '/') > 0
              THEN regexp_replace(lower(substring(t.name from strpos(t.name, '/') + 1)), '[^a-z0-9]+', '', 'g')
              ELSE '' END AS raw_repo
    FROM rh_topics t
    LEFT JOIN rh_topics ty ON ty.id = t.parent_id
   WHERE t.kind = 'entity'
), folded AS (
  SELECT id, parent_id, created_at, type_name,
         CASE WHEN char_length(raw_name) >= 8 AND right(raw_name, 2) = 'ql'
              THEN left(raw_name, char_length(raw_name) - 2) ELSE raw_name END AS name_key,
         CASE WHEN char_length(raw_slug) >= 8 AND right(raw_slug, 2) = 'ql'
              THEN left(raw_slug, char_length(raw_slug) - 2) ELSE raw_slug END AS slug_key,
         CASE WHEN char_length(raw_repo) >= 8 AND right(raw_repo, 2) = 'ql'
              THEN left(raw_repo, char_length(raw_repo) - 2) ELSE raw_repo END AS repo_key
    FROM ents
)
SELECT id, parent_id, created_at, type_name
  FROM folded
 WHERE name_key = ANY($1)
    OR slug_key = ANY($1)
    OR (char_length(repo_key) >= 6 AND repo_key = ANY($1))
UNION
SELECT t.id, t.parent_id, t.created_at, coalesce(ty.name, '') AS type_name
  FROM rh_topics t
  LEFT JOIN rh_topics ty ON ty.id = t.parent_id
  JOIN rh_topic_aliases a ON a.topic_id = t.id AND a.kind = 'entity'
  CROSS JOIN LATERAL (SELECT replace(lower(a.alias_key), '-', '') AS raw) ak
 WHERE t.kind = 'entity'
   AND (CASE WHEN char_length(ak.raw) >= 8 AND right(ak.raw, 2) = 'ql'
             THEN left(ak.raw, char_length(ak.raw) - 2) ELSE ak.raw END) = ANY($1)";

pub(super) const REMEMBER_TYPE: &str = "\
INSERT INTO rh_entity_type_seen
    (entity_id, summary_id, type_name, observed_at, owner_user_id)
SELECT $1, s.id, $3, $4, $5
  FROM rh_summaries s
 WHERE s.id = $2
ON CONFLICT (entity_id, summary_id, type_name) DO NOTHING";

pub(super) const RECENT_TYPES: &str = "\
SELECT type_name
  FROM rh_entity_type_seen
 WHERE entity_id = $1
 ORDER BY observed_at DESC, summary_id DESC, type_name DESC
 LIMIT $2";

pub(super) const CLAIMS_NAME: &str = "\
SELECT EXISTS (
    SELECT 1
      FROM rh_topics t
     WHERE t.kind = 'entity'
       AND t.id <> $1
       AND CASE
             WHEN char_length(regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g')) >= 8
              AND right(regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g'), 2) = 'ql'
             THEN left(
                    regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g'),
                    char_length(regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g')) - 2)
             ELSE regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g')
           END = $2
)";

pub(super) const HEAD: &str = "\
SELECT t.id, t.name, t.slug, t.owner_user_id, p.name AS type_name, p.slug AS type_slug
  FROM rh_topics t
  LEFT JOIN rh_topics p ON p.id = t.parent_id
 WHERE t.id = $1";

pub(super) const FINISH: &str = "\
INSERT INTO rh_entity_extractions
    (summary_id, status, pipeline_version, entities_touched, git_sha, error, extracted_at, owner_user_id)
VALUES ($1, 'done', $2, $3, $4, NULL, $5,
        (SELECT owner_user_id FROM rh_summaries WHERE id = $1))
ON CONFLICT (summary_id) DO UPDATE SET
    status = 'done',
    pipeline_version = EXCLUDED.pipeline_version,
    entities_touched = EXCLUDED.entities_touched,
    git_sha = EXCLUDED.git_sha,
    error = NULL,
    extracted_at = EXCLUDED.extracted_at";

pub(super) const FAIL: &str = "\
INSERT INTO rh_entity_extractions
    (summary_id, status, pipeline_version, entities_touched, error, extracted_at, owner_user_id)
VALUES ($1, 'failed', $2, '{}', $3, $4,
        (SELECT owner_user_id FROM rh_summaries WHERE id = $1))
ON CONFLICT (summary_id) DO UPDATE SET
    status = 'failed',
    pipeline_version = EXCLUDED.pipeline_version,
    error = EXCLUDED.error,
    extracted_at = EXCLUDED.extracted_at
 WHERE rh_entity_extractions.status <> 'done'
    OR rh_entity_extractions.pipeline_version < EXCLUDED.pipeline_version";

pub(super) const BACKFILL: &str = "\
SELECT s.id, s.conversation_id, coalesce(s.earliest_at, s.created_at) AS session_at
  FROM rh_summaries s
  LEFT JOIN rh_conversations c ON c.id = s.conversation_id
  LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
 WHERE s.kind = 'leaf'
   AND (c.id IS NULL OR c.session_key NOT LIKE 'heartbeat:%')
   AND (e.summary_id IS NULL OR e.status = 'failed' OR e.pipeline_version < $1)
   AND NOT EXISTS (
       SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = 'entity-ext-' || s.id::text)
   AND NOT EXISTS (
       SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'entity-ext-' || s.id::text)
 ORDER BY coalesce(s.earliest_at, s.created_at), s.id
 LIMIT greatest(0, (SELECT CASE WHEN EXISTS (
            SELECT 1 FROM graphile_worker._private_jobs n
            JOIN graphile_worker._private_tasks t ON t.id = n.task_id
            WHERE n.is_available AND n.run_at <= now()
              AND ((t.identifier = 'extract-entities' AND n.priority < $4)
                OR (t.identifier = 'compact-conversation' AND n.priority < 0)))
          THEN $3 ELSE $2 END)
        - (SELECT count(*) FROM graphile_worker._private_jobs q
            WHERE q.key LIKE 'entity-ext-%' AND q.priority = $4))";

pub(super) const STALE: &str = "\
SELECT s.id, s.conversation_id, coalesce(s.earliest_at, s.created_at) AS session_at
  FROM rh_summaries s
  LEFT JOIN rh_entity_extractions e ON e.summary_id = s.id
 WHERE s.kind = 'leaf'
   AND s.created_at > now() - interval '1 day'
   AND (e.summary_id IS NULL OR e.status <> 'done' OR e.pipeline_version < $1)
   AND NOT EXISTS (
       SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = 'entity-ext-' || s.id::text)
 ORDER BY coalesce(s.earliest_at, s.created_at), s.id
 LIMIT $2";

pub(super) const KNOWN_IDS: &str = "\
WITH keys AS MATERIALIZED (
    SELECT DISTINCT unnest($1::text[]) AS key
)
SELECT t.id
  FROM keys k
  JOIN rh_topics t
    ON t.kind = 'entity'
   AND regexp_replace(lower(t.name), '[^a-z0-9]+', '', 'g') = k.key
UNION
SELECT a.topic_id
  FROM keys k
  JOIN rh_topic_aliases a
    ON a.kind = 'entity'
   AND replace(lower(a.alias_key), '-', '') = k.key";

pub(super) const KNOWN_ROWS: &str = "\
SELECT t.id, t.name, t.created_at, p.name AS type_name
  FROM rh_topics t
  LEFT JOIN rh_topics p ON p.id = t.parent_id
 WHERE t.id = ANY($1) AND t.kind = 'entity'";

pub(super) const KNOWN_ALIASES: &str = "\
SELECT topic_id, alias
  FROM rh_topic_aliases
 WHERE topic_id = ANY($1) AND kind = 'entity'
 ORDER BY topic_id, alias_key";

pub(super) const NAME_ARM: &str = "\
WITH ents AS (
  SELECT e.id, e.name, ty.name AS type_name,
         regexp_replace(lower(e.name), '[^a-z0-9]+', '', 'g') AS raw_name,
         CASE WHEN strpos(e.name, '/') > 0
              THEN regexp_replace(lower(substring(e.name from strpos(e.name, '/') + 1)), '[^a-z0-9]+', '', 'g')
              ELSE '' END AS raw_repo
    FROM rh_topics e
    JOIN rh_topics ty ON ty.id = e.parent_id
   WHERE e.kind = 'entity'
), folded AS (
  SELECT id, name, type_name,
         CASE WHEN char_length(raw_name) >= 8 AND right(raw_name, 2) = 'ql'
              THEN left(raw_name, char_length(raw_name) - 2) ELSE raw_name END AS name_key,
         CASE WHEN char_length(raw_repo) >= 8 AND right(raw_repo, 2) = 'ql'
              THEN left(raw_repo, char_length(raw_repo) - 2) ELSE raw_repo END AS repo_key
    FROM ents
)
SELECT id, name, type_name, 1.0::float8 AS score
  FROM folded
 WHERE name_key = ANY($1)
    OR (char_length(repo_key) >= 6 AND repo_key = ANY($1))
UNION
SELECT e.id, e.name, ty.name AS type_name, 1.0::float8 AS score
  FROM rh_topics e
  JOIN rh_topics ty ON ty.id = e.parent_id
  JOIN rh_topic_aliases a ON a.topic_id = e.id AND a.kind = 'entity'
  CROSS JOIN LATERAL (SELECT replace(lower(a.alias_key), '-', '') AS raw) ak
 WHERE e.kind = 'entity'
   AND (CASE WHEN char_length(ak.raw) >= 8 AND right(ak.raw, 2) = 'ql'
             THEN left(ak.raw, char_length(ak.raw) - 2) ELSE ak.raw END) = ANY($1)
 ORDER BY score DESC, id
 LIMIT 48";

pub(super) const TRIGRAM_ARM: &str = "\
SELECT e.id, e.name, ty.name AS type_name,
       max(greatest(
           similarity(lower(e.name), lower(q.term)),
           word_similarity(lower(q.term), lower(e.name)),
           coalesce(similarity(lower(a.alias), lower(q.term)), 0),
           coalesce(word_similarity(lower(q.term), lower(a.alias)), 0)
       ))::float8 AS score
  FROM rh_topics e
  JOIN rh_topics ty ON ty.id = e.parent_id
  CROSS JOIN unnest($1::text[]) AS q(term)
  LEFT JOIN rh_topic_aliases a ON a.topic_id = e.id AND a.kind = 'entity'
 WHERE e.kind = 'entity'
 GROUP BY e.id, e.name, ty.name
 ORDER BY score DESC
 LIMIT 20";

pub(super) const EMBED_ARM: &str = "\
SELECT e.id, e.name, ty.name AS type_name,
       (1 - (emb.embedding <=> $1))::float8 AS score
  FROM rh_entity_embeddings emb
  JOIN rh_topics e ON e.id = emb.entity_id
  JOIN rh_topics ty ON ty.id = e.parent_id
 WHERE e.kind = 'entity'
 ORDER BY emb.embedding <=> $1
 LIMIT 10";

pub(super) const MARKER_ARM: &str = "\
SELECT id, name, type_name, score FROM (
    SELECT e.id, e.name, ty.name AS type_name, 1.0::float8 AS score
      FROM rh_topics e
      JOIN rh_topics ty ON ty.id = e.parent_id
      JOIN rh_entity_facts f ON f.topic_id = e.id
      JOIN rh_entity_fact_claims c ON c.fact_id = f.id
      CROSS JOIN unnest($1::text[]) AS q(term)
     WHERE e.kind = 'entity'
       AND char_length(q.term) >= 3
       AND position(lower(q.term) in lower(c.value)) > 0
    UNION
    SELECT e.id, e.name, ty.name AS type_name, 1.0::float8 AS score
      FROM rh_topics e
      JOIN rh_topics ty ON ty.id = e.parent_id
      JOIN rh_wiki_topics w ON w.topic_id = e.id
      CROSS JOIN unnest($1::text[]) AS q(term)
     WHERE e.kind = 'entity'
       AND char_length(q.term) >= 3
       AND (
            position(lower(q.term) in lower(coalesce(w.current_state, ''))) > 0
            OR position(lower(q.term) in lower(left(coalesce(w.article, ''), 4000))) > 0
       )
) hits
 LIMIT 48";

pub(super) const PAGE_TEXT: &str = "\
SELECT DISTINCT ON (topic_id)
       topic_id, coalesce(current_state, '') AS current_state,
       left(coalesce(article, ''), 400) AS article
  FROM rh_wiki_topics
 WHERE topic_id = ANY($1)
 ORDER BY topic_id, updated_at DESC";

pub(super) const RECORD_DECISION: &str = "\
INSERT INTO rh_entity_decisions
    (id, summary_id, kind, proposed_name, proposed_type, candidates, verdict, choice, confidence,
     model, entity_id, created_at, owner_user_id)
SELECT $1, s.id, $3, $4, $5, $6::jsonb, $7, $8, $9, $10, $11, $12, s.owner_user_id
  FROM rh_summaries s
 WHERE s.id = $2";

pub(super) const TYPE_ALIASES: &str = "\
SELECT topic_id, alias
  FROM rh_topic_aliases
 WHERE kind = 'type' AND topic_id = ANY($1)
 ORDER BY topic_id, alias_key";

pub(super) const TYPE_EXAMPLES: &str = "\
SELECT parent_id, name
  FROM (
    SELECT e.parent_id, e.name,
           row_number() OVER (PARTITION BY e.parent_id ORDER BY e.updated_at DESC, e.id) AS n
      FROM rh_topics e
     WHERE e.kind = 'entity' AND e.parent_id = ANY($1)
  ) ranked
 WHERE n <= 3
 ORDER BY parent_id, n";

pub(super) const ALIAS_LINKS: &str = "\
SELECT topic_id, alias
  FROM rh_topic_aliases
 WHERE topic_id = ANY($1) AND kind = 'entity'
 ORDER BY topic_id, alias_key";

pub(super) const REMEMBER: &str = "\
INSERT INTO rh_entity_embeddings (entity_id, embedding, embedded_at, owner_user_id)
SELECT $1, $2, $3, coalesce(owner_user_id, '')
  FROM rh_topics WHERE id = $1
ON CONFLICT (entity_id) DO UPDATE
   SET embedding = EXCLUDED.embedding,
       embedded_at = EXCLUDED.embedded_at";

pub(super) const HOLD: &str = "\
INSERT INTO rh_entity_candidates
    (id, summary_id, name, type_name, confidence, reason, payload, created_at, owner_user_id)
SELECT $1, s.id, $3, $4, $5, $6, $7, $8, s.owner_user_id
  FROM rh_summaries s
 WHERE s.id = $2";
