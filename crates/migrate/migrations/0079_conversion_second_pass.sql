DO $rivet$
BEGIN
    SET LOCAL lock_timeout = '50ms';
    IF EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0079_conversion_second_pass.sql') THEN
        RETURN;
    END IF;

    ALTER TABLE rh_summary_conversion_items ADD COLUMN second_pass_at timestamptz;
    INSERT INTO _rivetos_migrations (name) VALUES ('0079_conversion_second_pass.sql');
END
$rivet$;
