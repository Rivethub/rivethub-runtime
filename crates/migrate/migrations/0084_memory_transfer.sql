DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0084_memory_transfer.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_topic_tombstones ADD COLUMN owner_user_id text;
    CREATE FUNCTION rh_capture_topic_tombstone_owner() RETURNS trigger
        LANGUAGE plpgsql
        AS $owner$
        BEGIN
            IF NEW.owner_user_id IS NULL THEN
                SELECT owner_user_id INTO NEW.owner_user_id FROM rh_topics
                WHERE kind = NEW.kind AND slug = NEW.slug;
            END IF;
            RETURN NEW;
        END
        $owner$;
    CREATE TRIGGER rh_capture_topic_tombstone_owner BEFORE INSERT ON rh_topic_tombstones
        FOR EACH ROW EXECUTE FUNCTION rh_capture_topic_tombstone_owner();

    ALTER TABLE rh_wiki_redirects ADD COLUMN owner_user_id text;
    ALTER TABLE rh_wiki_redirects ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal();

    ALTER TABLE rh_wiki_topics ADD COLUMN archived_sections text;
    ALTER TABLE rh_topics ALTER CONSTRAINT rh_topics_parent_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    ALTER TABLE rh_summaries ALTER CONSTRAINT ros_summaries_parent_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    ALTER TABLE rh_tasks ALTER CONSTRAINT rh_tasks_parent_task_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    INSERT INTO _rivetos_migrations (name) VALUES ('0084_memory_transfer.sql');
END
$rivet$;
