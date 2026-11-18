-- Reuse complete windows for identical owner text. An unchanged logical slot
-- with the same ordered evidence keeps the physical passage ID.
WITH source AS (
    SELECT old.id, old.kind = 'session' AND dest.kind = 'session'
        AND ARRAY(SELECT message_id FROM rh_summary_sources WHERE summary_id = old.id ORDER BY ordinal)
          = ARRAY(SELECT message_id FROM rh_summary_sources WHERE summary_id = dest.id ORDER BY ordinal)
        AS keep_id
    FROM rh_summaries dest JOIN rh_summaries old
      ON old.conversation_id = dest.conversation_id AND old.owner_user_id = dest.owner_user_id
     AND old.content = dest.content AND old.index_key = dest.index_key
    WHERE dest.id = $1 AND old.id = ANY($2)
      AND old.kind IN ('session', 'part') AND NOT old.index_needs_repair
      AND rh_summary_record_indexed(old.id)
      AND NOT EXISTS (SELECT 1 FROM rh_summary_passages p
          WHERE p.record_id = old.id AND (p.index_key IS DISTINCT FROM dest.index_key
             OR p.content IS DISTINCT FROM substring(dest.content FROM p.char_start + 1 FOR p.char_end - p.char_start)))
    ORDER BY keep_id DESC, old.id
    LIMIT 1
), moved AS (
    UPDATE rh_summary_passages p SET record_id = $1
    FROM source WHERE source.keep_id AND p.record_id = source.id
    RETURNING p.id
), copied AS (
    INSERT INTO rh_summary_passages (id, record_id, ordinal, char_start, char_end, content,
        earliest_at, latest_at, index_key, embedding)
    SELECT gen_random_uuid(), dest.id, p.ordinal, p.char_start, p.char_end, p.content,
        coalesce(evidence.earliest_at, dest.earliest_at),
        coalesce(evidence.latest_at, dest.latest_at), p.index_key, p.embedding
    FROM source JOIN rh_summary_passages p ON p.record_id = source.id
    JOIN rh_summaries dest ON dest.id = $1
    LEFT JOIN LATERAL (
        SELECT min(m.created_at) AS earliest_at, max(m.created_at) AS latest_at
        FROM rh_summaries s
        CROSS JOIN LATERAL rh_summary_message_ids(s.id) membership
        JOIN rh_messages m ON m.id = membership.message_id
        WHERE s.parent_id = dest.id AND s.kind = 'section'
          AND p.char_start < s.section_char_end AND p.char_end > s.section_char_start
    ) evidence ON true
    WHERE NOT source.keep_id
    RETURNING id
)
UPDATE rh_summaries SET index_needs_repair = false
WHERE id = $1 AND (EXISTS (SELECT 1 FROM moved) OR EXISTS (SELECT 1 FROM copied))
