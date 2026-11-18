WITH dest AS (
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
