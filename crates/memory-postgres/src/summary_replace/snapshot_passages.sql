CREATE TEMP TABLE summary_old_passages ON COMMIT DROP AS
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
