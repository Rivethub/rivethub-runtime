DO $rivet$
BEGIN
    SET LOCAL lock_timeout = '50ms';
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0078_summary_index_spec.sql') THEN
        RETURN;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE OR REPLACE FUNCTION rh_summary_record_indexed(node_id uuid)
    RETURNS boolean LANGUAGE sql STABLE SECURITY INVOKER AS $indexed$
        SELECT coalesce(bool_and(embedded AND char_start = previous_end AND ordinal = position)
                        AND max(char_end) = max(content_length), false)
        FROM (
            SELECT p.char_start, p.char_end, p.ordinal, length(s.content) AS content_length,
                   p.embedding IS NOT NULL AND p.index_key = s.index_key
                       AND p.content = substring(s.content FROM p.char_start + 1 FOR p.char_end - p.char_start)
                       AS embedded,
                   lag(p.char_end, 1, 0) OVER (ORDER BY p.ordinal) AS previous_end,
                   row_number() OVER (ORDER BY p.ordinal) - 1 AS position
            FROM rh_summary_passages p JOIN rh_summaries s ON s.id = p.record_id
            WHERE p.record_id = node_id AND s.kind IN ('session', 'part') AND s.index_key IS NOT NULL
        ) pieces
    $indexed$;

    UPDATE rh_summary_passages p SET embedding = NULL
    FROM rh_summaries s WHERE s.id = p.record_id AND p.embedding IS NOT NULL
      AND p.index_key IS DISTINCT FROM s.index_key;

    UPDATE rh_summaries s SET index_needs_repair = true
    WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
      AND NOT rh_summary_record_indexed(s.id);

    INSERT INTO _rivetos_migrations (name) VALUES ('0078_summary_index_spec.sql');
END
$rivet$;
