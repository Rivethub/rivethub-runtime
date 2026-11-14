DO $rivet$
BEGIN
    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0075_wiki_scope_claims.sql') THEN
            RETURN;
        END IF;
    END IF;
    PERFORM set_config('rh.maintenance', 'on', true);

    CREATE POLICY rh_claim ON rh_wiki_scopes FOR INSERT WITH CHECK (
        owner_user_id = scope
        AND ((visibility = 'private' AND scope = (SELECT rh_scope_principal()))
             OR (visibility <> 'private' AND scope = visibility
                 AND visibility = ANY ((SELECT rh_scope_spaces())::text[]))));

    IF to_regclass('_rivetos_migrations') IS NOT NULL THEN
        INSERT INTO _rivetos_migrations (name) VALUES ('0075_wiki_scope_claims.sql');
    END IF;
END
$rivet$;
