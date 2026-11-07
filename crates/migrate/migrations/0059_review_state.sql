DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0059_review_state.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_review_marks (
        owner_user_id text NOT NULL CHECK (owner_user_id <> ''),
        item_key text NOT NULL CHECK (item_key <> '' AND char_length(item_key) <= 512),
        state text NOT NULL CHECK (state IN ('decided', 'snoozed')),
        at timestamptz NOT NULL,
        PRIMARY KEY (owner_user_id, item_key)
    );

    CREATE POLICY rh_own ON rh_review_marks USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_review_marks ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_review_marks FORCE ROW LEVEL SECURITY;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0059_review_state.sql');
    END IF;
END
$rivet$;
