DO $rivet$
BEGIN
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0084_memory_transfer.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_topic_tombstones ADD COLUMN owner_user_id text;

    ALTER TABLE rh_wiki_redirects ADD COLUMN owner_user_id text;
    ALTER TABLE rh_wiki_redirects ALTER COLUMN owner_user_id SET DEFAULT rh_scope_principal();

    ALTER TABLE rh_wiki_topics ADD COLUMN archived_sections text;
    ALTER TABLE rh_topics ALTER CONSTRAINT rh_topics_parent_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    ALTER TABLE rh_summaries ALTER CONSTRAINT ros_summaries_parent_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    ALTER TABLE rh_tasks ALTER CONSTRAINT rh_tasks_parent_task_id_fkey DEFERRABLE INITIALLY IMMEDIATE;
    INSERT INTO _rivetos_migrations (name) VALUES ('0084_memory_transfer.sql');
END
$rivet$;
