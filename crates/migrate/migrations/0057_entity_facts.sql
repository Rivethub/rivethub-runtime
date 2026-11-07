DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0057_entity_facts.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE TABLE rh_entity_facts (
        id uuid PRIMARY KEY,
        topic_id uuid NOT NULL REFERENCES rh_topics (id) ON DELETE CASCADE,
        key text NOT NULL CHECK (key <> '' AND char_length(key) <= 64),
        created_at timestamptz NOT NULL
    );
    CREATE INDEX rh_entity_facts_key ON rh_entity_facts (topic_id, key, created_at);
    CREATE TABLE rh_entity_fact_claims (
        id uuid PRIMARY KEY,
        fact_id uuid NOT NULL REFERENCES rh_entity_facts (id) ON DELETE CASCADE,
        value text NOT NULL CHECK (value <> '' AND char_length(value) <= 400),
        actor text NOT NULL CHECK (actor IN ('you', 'wiki-writer')),
        pinned boolean NOT NULL DEFAULT false,
        session_id text CHECK (session_id <> ''),
        summary_id uuid,
        evidence text CHECK (char_length(evidence) <= 300),
        at timestamptz NOT NULL,
        recorded_at timestamptz NOT NULL,
        owner_user_id text NOT NULL,
        CHECK (actor = 'you' OR NOT pinned),
        CHECK ((actor = 'wiki-writer') = (session_id IS NOT NULL AND summary_id IS NOT NULL)),
        CHECK (actor = 'wiki-writer' OR (session_id IS NULL AND summary_id IS NULL AND evidence IS NULL))
    );
    CREATE INDEX rh_entity_fact_claims_fact ON rh_entity_fact_claims (fact_id, at);
    CREATE INDEX rh_entity_fact_claims_session ON rh_entity_fact_claims (session_id)
        WHERE session_id IS NOT NULL;
    CREATE UNIQUE INDEX rh_entity_fact_claims_summary ON rh_entity_fact_claims (fact_id, summary_id)
        WHERE summary_id IS NOT NULL;

    CREATE POLICY rh_read ON rh_entity_facts FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_topics t WHERE t.id = topic_id));
    CREATE POLICY rh_write ON rh_entity_facts USING (
        (SELECT rh_scope_maintenance()) OR EXISTS (SELECT 1 FROM rh_topics t WHERE t.id = topic_id));
    CREATE POLICY rh_read ON rh_entity_fact_claims FOR SELECT USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal())
        OR (EXISTS (SELECT 1 FROM rh_entity_facts f WHERE f.id = fact_id)
            AND (session_id IS NULL
                 OR EXISTS (SELECT 1 FROM rh_conversations c
                             WHERE c.session_key IN (session_id, substring(session_id FROM '^claude-code:(.+)$'))
                               AND c.visibility <> 'private'))));
    CREATE POLICY rh_write ON rh_entity_fact_claims USING (
        (SELECT rh_scope_maintenance()) OR owner_user_id = (SELECT rh_scope_principal()));
    ALTER TABLE rh_entity_facts ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_facts FORCE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_fact_claims ENABLE ROW LEVEL SECURITY;
    ALTER TABLE rh_entity_fact_claims FORCE ROW LEVEL SECURITY;

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0057_entity_facts.sql');
    END IF;
END
$rivet$;
