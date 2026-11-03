use std::fs;
use std::process::Command;

use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use sqlx::Row as _;
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn recording(name: &str) -> std::path::PathBuf {
    testkit::recording(&format!("memory-sqlite/{name}"))
}

fn run(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_rivethub-migrate"))
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "rivethub-migrate {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn migrating_a_v5_copy_reports_v8_and_leaves_a_byte_equal_backup() -> TestResult {
    let original = fs::read(recording("upgrade-v5.sqlite"))?;
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    fs::copy(recording("upgrade-v5.sqlite"), &path)?;

    let stdout = run(&["sqlite", "--path", &path.to_string_lossy()])?;
    assert!(
        stdout.contains("current 30")
            && stdout.contains("required 30")
            && stdout.contains("pending 0"),
        "the applied report must name the reached state: {stdout}"
    );

    let backup = dir.path().join("memory.sqlite.v5.bak");
    assert!(
        backup.exists(),
        "the pre-upgrade copy must be <file>.v5.bak"
    );
    assert_eq!(
        fs::read(&backup)?,
        original,
        "the backup must be byte-equal to the upgraded file's original bytes"
    );

    let conn = rusqlite::Connection::open(&path)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(version, 30);
    Ok(())
}

#[test]
fn status_reports_the_sqlite_ladder_position() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    fs::copy(recording("upgrade-v5.sqlite"), &path)?;

    let stdout = run(&["status", "sqlite", "--path", &path.to_string_lossy()])?;
    assert!(
        stdout.contains("current 5")
            && stdout.contains("required 30")
            && stdout.contains("pending 25"),
        "status must report the current, required and pending steps: {stdout}"
    );
    assert!(
        !dir.path().join("memory.sqlite.v5.bak").exists(),
        "status must not touch the file"
    );

    let missing = dir.path().join("absent.sqlite");
    let stdout = run(&["status", "sqlite", "--path", &missing.to_string_lossy()])?;
    assert!(
        stdout.contains("current 0") && stdout.contains("pending 30"),
        "a missing file reads as version 0 with the whole ladder pending: {stdout}"
    );
    assert!(!missing.exists(), "status must not create the file");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn status_reports_a_migrated_postgres_database(pool: PgPool) -> TestResult {
    let url = pool.connect_options().to_url_lossy().to_string();
    let stdout = run(&["status", "postgres", "--url", &url])?;
    assert!(
        stdout.contains("current 52")
            && stdout.contains("required 52")
            && stdout.contains("pending 0"),
        "status must report the applied state: {stdout}"
    );
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn status_reports_a_fresh_postgres_database_and_migrate_applies_the_chain(
    pool: PgPool,
) -> TestResult {
    let url = pool.connect_options().to_url_lossy().to_string();
    let stdout = run(&["status", "postgres", "--url", &url])?;
    assert!(
        stdout.contains("current 0")
            && stdout.contains("required 52")
            && stdout.contains("pending 48"),
        "a fresh database reads as version 0 with the whole chain pending: {stdout}"
    );

    let stdout = run(&["postgres", "--url", &url])?;
    assert!(
        stdout.contains("current 52") && stdout.contains("pending 0"),
        "migrate must apply the chain and report the reached state: {stdout}"
    );
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(tables > 0, "the applied chain must have created tables");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn adopting_a_database_without_sqlx_history_changes_no_columns(pool: PgPool) -> TestResult {
    sqlx::query("DROP TABLE _sqlx_migrations")
        .execute(&pool)
        .await?;
    let url = pool.connect_options().to_url_lossy().to_string();
    match memory_postgres::PostgresMemory::connect(&url).await {
        Err(memory_postgres::Error::NotAdopted) => {}
        other => panic!("expected NotAdopted, got {other:?}"),
    }
    let before = column_listing(&pool).await?;
    let stdout = run(&["postgres", "--url", &url])?;
    assert!(
        stdout.contains("current 52") && stdout.contains("pending 0"),
        "adopting must succeed: {stdout}"
    );
    let after = column_listing(&pool).await?;
    assert_eq!(before, after, "adopting must apply no schema change");
    memory_postgres::PostgresMemory::connect(&url).await?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn config_warnings_print_once_and_config_parses_after_the_subcommand(
    pool: PgPool,
) -> TestResult {
    let url = pool.connect_options().to_url_lossy().to_string();
    let dir = tempdir()?;
    let config = dir.path().join("config.yaml");
    let workspace = dir.path().join("workspace");
    fs::create_dir(&workspace)?;
    let text = format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    model: m\n    base_url: http://127.0.0.1:9/v1\nmemory:\n  postgres:\n    embedded:\n      auto_migrate: true\n",
        workspace.display()
    );
    fs::write(&config, text)?;
    let config = config.to_string_lossy();
    for args in [
        vec!["postgres", "--config", config.as_ref()],
        vec!["--config", config.as_ref(), "postgres"],
    ] {
        let output = invoke(&args, &[("RIVETOS_PG_URL", url.as_str())])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "rivethub-migrate {} must parse and apply: {stderr}",
            args.join(" ")
        );
        assert_eq!(
            stderr
                .matches("auto_migrate is gone; run rivethub-migrate")
                .count(),
            1,
            "the removed-key warning must print once: {stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("current 52") && stdout.contains("pending 0"),
            "the config's postgres url must be used: {stdout}"
        );
    }
    Ok(())
}

fn invoke(
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rivethub-migrate"));
    command.args(args).env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    Ok(command.output()?)
}

async fn column_listing(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT table_schema, table_name, column_name, ordinal_position::text,
                data_type, udt_name, is_nullable,
                coalesce(character_maximum_length::text, ''),
                coalesce(numeric_precision::text, ''),
                coalesce(numeric_scale::text, '')
         FROM information_schema.columns
         WHERE table_schema <> 'pg_catalog'
           AND table_schema <> 'information_schema'
           AND table_name <> '_sqlx_migrations'
         ORDER BY table_schema, table_name, ordinal_position, column_name",
    )
    .fetch_all(pool)
    .await?;
    let mut lines = Vec::new();
    for row in rows {
        let mut fields = Vec::new();
        for index in 0..10 {
            fields.push(row.try_get::<String, _>(index)?);
        }
        lines.push(fields.join("|"));
    }
    Ok(lines)
}

#[test]
fn config_validation_errors_name_the_key_and_rule() -> TestResult {
    let directory = tempdir()?;
    let path = directory.path().join("config.yaml");
    for (text, key, rule) in [
        (
            "runtime: {}\nagents: {}\nproviders: {}\n",
            "runtime.workspace",
            "required",
        ),
        (
            "runtime:\n  workspace: 42\n",
            "runtime.workspace",
            "wrong type",
        ),
    ] {
        fs::write(&path, text)?;
        let output = invoke(&["sqlite", "--config", &path.to_string_lossy()], &[])?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains(key), "{stderr}");
        assert!(stderr.contains(rule), "{stderr}");
        assert!(!directory.path().join("memory.sqlite").exists());
    }
    Ok(())
}
