DO $rivet$
BEGIN
    SET LOCAL lock_timeout = '50ms';
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0080_opening_checked_user_count.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_summary_state ADD COLUMN opening_checked_user_count bigint;
    INSERT INTO _rivetos_migrations (name) VALUES ('0080_opening_checked_user_count.sql');
END
$rivet$;
