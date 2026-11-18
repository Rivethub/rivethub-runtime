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
fn sqlite_status_reports_the_ladder_position_and_migrate_reaches_the_top() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    fs::copy(recording("upgrade-v5.sqlite"), &path)?;
    let shown = path.display();
    let file = path.to_string_lossy();
    let required = memory_sqlite::SCHEMA_VERSION;
    assert_eq!(
        run(&["status", "sqlite", "--path", &file])?,
        format!(
            "sqlite {shown}: current 5, required {required}, pending {}\n",
            required - 5
        )
    );
    assert_eq!(
        run(&["sqlite", "--path", &file])?,
        format!("sqlite {shown}: applied; current {required}, required {required}, pending 0\n")
    );
    Ok(())
}

#[test]
fn sqlite_imports_a_pg_dump_and_reports_its_rows() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("memory.sqlite");
    let stdout = run(&[
        "sqlite",
        "--path",
        &path.to_string_lossy(),
        "--import-pg-dump",
        &recording("import/copy-cases.txt").to_string_lossy(),
    ])?;
    let report: memory_sqlite::import::ImportReport = serde_json::from_str(&stdout)?;
    assert_eq!(
        report
            .rows
            .get(&memory_sqlite::import::ImportTable::Summaries),
        Some(&2)
    );
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn postgres_status_reports_a_fresh_database_and_migrate_applies_the_chain(
    pool: PgPool,
) -> TestResult {
    let url = pool.connect_options().to_url_lossy().to_string();
    let required = migrate::required()?;
    assert_eq!(
        run(&["status", "postgres", "--url", &url])?,
        format!(
            "postgres: current 0, required {required}, pending {}\n",
            migrate::POSTGRES.migrations.len()
        )
    );
    assert_eq!(
        run(&["postgres", "--url", &url])?,
        format!("postgres: applied; current {required}, required {required}, pending 0\n")
    );
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
    let required = migrate::required()?;
    assert_eq!(
        run(&["postgres", "--url", &url])?,
        format!("postgres: applied; current {required}, required {required}, pending 0\n")
    );
    assert_eq!(column_listing(&pool).await?, before);
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
            stdout.contains("current 77") && stdout.contains("pending 0"),
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
