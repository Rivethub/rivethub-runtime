DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0071_self_healing_message_counts.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE FUNCTION rh_message_count_recount(conversations uuid[]) RETURNS void
        LANGUAGE plpgsql VOLATILE
        AS $$
        BEGIN
            DELETE FROM rh_message_counts WHERE conversation_id = ANY (conversations);
            INSERT INTO rh_message_counts
                   (conversation_id, owner_user_id, tool_name, total, tool_calls, embedded, queued, failed)
            SELECT m.conversation_id, m.owner_user_id, coalesce(m.tool_name, ''), count(*),
                   count(*) FILTER (WHERE m.role = 'tool' OR m.tool_name IS NOT NULL),
                   count(m.embedding),
                   count(*) FILTER (WHERE m.embedding IS NULL AND m.embed_status IS DISTINCT FROM 'unembeddable'
                                      AND m.embed_status IS DISTINCT FROM 'failed'
                                      AND (m.content <> '' OR m.tool_result <> '')),
                   count(*) FILTER (WHERE m.embed_status = 'failed')
              FROM rh_messages m WHERE m.conversation_id = ANY (conversations)
             GROUP BY 1, 2, 3;
        END $$;
    REVOKE EXECUTE ON FUNCTION rh_message_count_recount(uuid[]) FROM PUBLIC;

    CREATE OR REPLACE FUNCTION rh_message_count_add(changed jsonb) RETURNS void
        LANGUAGE plpgsql VOLATILE
        AS $$
        DECLARE
            broken uuid[];
        BEGIN
            INSERT INTO rh_message_counts AS c
                   (conversation_id, owner_user_id, tool_name, total, tool_calls, embedded, queued, failed)
            SELECT d.conversation_id, d.owner_user_id, d.tool_name,
                   sum(d.total), sum(d.tool_calls), sum(d.embedded), sum(d.queued), sum(d.failed)
              FROM jsonb_to_recordset(changed) AS d(conversation_id uuid, owner_user_id text, tool_name text,
                       total bigint, tool_calls bigint, embedded bigint, queued bigint, failed bigint)
             WHERE NOT EXISTS (SELECT 1 FROM rh_message_counts_pending p
                                WHERE p.conversation_id = d.conversation_id)
             GROUP BY d.conversation_id, d.owner_user_id, d.tool_name
            HAVING sum(d.total) <> 0 OR sum(d.tool_calls) <> 0 OR sum(d.embedded) <> 0
                OR sum(d.queued) <> 0 OR sum(d.failed) <> 0
             ORDER BY d.conversation_id, d.owner_user_id, d.tool_name
            ON CONFLICT (conversation_id, owner_user_id, tool_name) DO UPDATE
               SET total = c.total + EXCLUDED.total, tool_calls = c.tool_calls + EXCLUDED.tool_calls,
                   embedded = c.embedded + EXCLUDED.embedded, queued = c.queued + EXCLUDED.queued,
                   failed = c.failed + EXCLUDED.failed;
            SELECT array_agg(DISTINCT n.conversation_id) INTO broken
              FROM rh_message_counts n
             WHERE n.conversation_id IN (SELECT (d ->> 'conversation_id')::uuid
                                           FROM jsonb_array_elements(changed) AS d)
               AND (n.total < 0 OR n.tool_calls < 0 OR n.embedded < 0 OR n.queued < 0 OR n.failed < 0);
            IF broken IS NOT NULL THEN
                PERFORM rh_message_count_recount(broken);
            END IF;
        END $$;

    PERFORM rh_message_count_recount(array_agg(DISTINCT conversation_id))
       FROM rh_message_counts
      WHERE total < 0 OR tool_calls < 0 OR embedded < 0 OR queued < 0 OR failed < 0
     HAVING count(*) > 0;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0071_self_healing_message_counts.sql');
    END IF;
END
$rivet$;
