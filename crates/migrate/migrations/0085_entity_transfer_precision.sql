DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0085_entity_transfer_precision.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_entity_embeddings ADD COLUMN sqlite_embedding bytea
        CHECK (octet_length(sqlite_embedding) = 4096);

    CREATE FUNCTION rh_clear_transferred_embedding() RETURNS trigger
        LANGUAGE plpgsql
        AS $embedding$
        BEGIN
            NEW.sqlite_embedding := NULL;
            RETURN NEW;
        END
        $embedding$;
    CREATE TRIGGER rh_clear_transferred_embedding BEFORE UPDATE OF embedding ON rh_entity_embeddings
        FOR EACH ROW EXECUTE FUNCTION rh_clear_transferred_embedding();

    INSERT INTO _rivetos_migrations (name) VALUES ('0085_entity_transfer_precision.sql');
END
$rivet$;
