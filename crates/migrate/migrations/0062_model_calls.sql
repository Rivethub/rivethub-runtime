DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0062_model_calls.sql') THEN
            RETURN;
        END IF;
    END IF;

    CREATE TABLE IF NOT EXISTS rh_model_calls (
        call_id text PRIMARY KEY CHECK (call_id <> ''),
        owner_user_id text NOT NULL,
        session_key text,
        turn_id text,
        request_kind text,
        started_at timestamptz NOT NULL,
        finished_at timestamptz,
        provider text NOT NULL,
        model_requested text,
        model_served text,
        account text,
        status_code integer,
        outcome text NOT NULL CHECK (outcome IN ('completed', 'rate_limited', 'client_error', 'server_error', 'aborted')),
        retry_after_secs bigint,
        input_tokens bigint,
        output_tokens bigint,
        cache_read_tokens bigint,
        cache_write_tokens bigint,
        reasoning_tokens bigint
    );
    CREATE INDEX IF NOT EXISTS rh_model_calls_session ON rh_model_calls (session_key, started_at);

    ALTER TABLE rh_model_calls ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_model_calls FORCE ROW LEVEL SECURITY;
    CREATE POLICY rh_own ON rh_model_calls USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0062_model_calls.sql');
    END IF;
END
$rivet$;
