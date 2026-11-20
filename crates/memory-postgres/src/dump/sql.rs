pub(super) const SUMMARY_GRAPH: &str = r"WITH nodes AS (
    SELECT s.* FROM rh_summaries s WHERE s.pipeline_version >= 7
        OR s.kind IN ('session', 'part', 'section')
), coverage AS (
    SELECT s.id, count(m.id) AS source_count, min(l.ordinal) AS first, max(l.ordinal) AS last,
           count(DISTINCT l.ordinal) AS ordinals, min(m.created_at) AS earliest, max(m.created_at) AS latest
    FROM nodes s
    LEFT JOIN rh_summary_sources l ON l.summary_id =
        CASE WHEN s.kind = 'section' THEN s.parent_id ELSE s.id END
        AND (s.kind <> 'section' OR l.ordinal BETWEEN s.source_first AND s.source_last)
    LEFT JOIN rh_messages m ON m.id = l.message_id AND m.conversation_id = s.conversation_id AND m.owner_user_id = s.owner_user_id
    GROUP BY s.id
)
SELECT EXISTS (
    SELECT 1 FROM nodes s
    LEFT JOIN nodes p ON p.id = s.parent_id AND p.conversation_id = s.conversation_id AND p.owner_user_id = s.owner_user_id
    LEFT JOIN rh_conversations conv ON conv.id = s.conversation_id
    JOIN coverage c ON c.id = s.id
    WHERE (
        CASE s.kind
            WHEN 'session' THEN s.parent_id IS NULL AND s.depth = 2 AND s.summary_ordinal = 0
                AND s.source_first = 1 AND s.source_last = s.message_count
            WHEN 'part' THEN p.kind = 'session' AND s.depth = 1 AND s.summary_ordinal > 0
                AND s.issued_part_ordinal > 0 AND s.input_token_estimate > 0
                AND NOT EXISTS (
                    SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = s.id
                    AND NOT EXISTS (SELECT 1 FROM rh_summary_sources r
                        WHERE r.summary_id = p.id AND r.message_id = l.message_id)
                )
            WHEN 'section' THEN p.kind IN ('session', 'part') AND s.depth = 0
                AND s.range_verified IS NOT NULL
                AND (s.range_verified OR (s.source_first = p.source_first AND s.source_last = p.source_last))
                AND s.section_char_start >= 0 AND s.section_char_end > s.section_char_start
                AND s.section_char_end <= length(p.content)
                AND NOT EXISTS (SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = s.id)
            ELSE false
        END
        AND (s.kind = 'section' OR (s.range_verified IS NULL AND s.section_char_start IS NULL
            AND s.section_char_end IS NULL))
        AND s.owner_user_id = conv.owner_user_id
        AND s.pipeline_version = 7 AND s.summary_ordinal >= 0 AND length(s.input_key) = 32
        AND s.quality_flags >= 0 AND s.source_first > 0
        AND s.message_count > 0 AND s.message_count = c.source_count AND c.source_count = c.ordinals
        AND s.source_first = c.first AND s.source_last = c.last
        AND s.earliest_at = c.earliest AND s.latest_at = c.latest
    ) IS NOT TRUE
    UNION ALL
    SELECT 1 FROM nodes WHERE parent_id IS NOT NULL
    GROUP BY parent_id, kind, summary_ordinal HAVING count(*) > 1
    UNION ALL
    SELECT 1 FROM rh_summary_passages piece
    LEFT JOIN nodes s ON s.id = piece.record_id AND s.kind IN ('session', 'part')
    WHERE s.id IS NULL OR piece.char_end > length(s.content)
        OR piece.earliest_at < s.earliest_at OR piece.latest_at > s.latest_at
        OR substring(s.content FROM piece.char_start + 1 FOR piece.char_end - piece.char_start) <> piece.content
    UNION ALL
    SELECT 1 FROM rh_summary_deliveries d
    LEFT JOIN rh_messages m ON m.id = d.message_id AND m.conversation_id = d.conversation_id
    LEFT JOIN nodes s ON s.id = d.anchor_id AND s.conversation_id = d.conversation_id
    WHERE m.id IS NULL OR (d.anchor_id IS NOT NULL AND (
        s.id IS NULL OR s.kind NOT IN ('session', 'part') OR NOT EXISTS (
            SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = d.anchor_id AND l.message_id = d.message_id
        )
    ))
)
";
