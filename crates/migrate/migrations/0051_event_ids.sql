DO $rivet$
DECLARE
    leaky boolean;
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0051_event_ids.sql') THEN
            RETURN;
        END IF;
    END IF;

    IF (SELECT rolsuper FROM pg_roles WHERE rolname = current_user) THEN
        ALTER FUNCTION jsonb_object_field_text(jsonb, text) LEAKPROOF;
    END IF;
    SELECT NOT proleakproof INTO leaky FROM pg_proc
     WHERE oid = 'jsonb_object_field_text(jsonb,text)'::regprocedure;
    IF leaky THEN
        RAISE EXCEPTION 'migration 0051 indexes each message''s capture event id, and row-level security lets capture use that index only when jsonb_object_field_text (the ->> operator) is LEAKPROOF. A superuser marks it once per database: ALTER FUNCTION jsonb_object_field_text(jsonb, text) LEAKPROOF; Then run the migration again.';
    END IF;

    CREATE INDEX IF NOT EXISTS rh_messages_event_id
        ON rh_messages (conversation_id, (metadata ->> 'event_id'))
        WHERE (metadata ->> 'event_id') IS NOT NULL;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0051_event_ids.sql');
    END IF;
END
$rivet$;
