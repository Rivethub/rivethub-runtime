#![cfg(test)]

use crate::support;

use cli::Exit;
use sqlx::ConnectOptions as _;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn reembed_dry_run_through_a_symlink_sees_the_wal() {
    let home = support::home();
    let path = home.path().join("scratch.sqlite");
    let alias = home.path().join("alias.sqlite");
    migrate::sqlite(&path).expect("schema");
    std::os::unix::fs::symlink(&path, &alias).expect("symlink");
    let writer = rusqlite::Connection::open(&path).expect("writer");
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             INSERT INTO ros_wiki_topics (slug, title, article, embedding, created_at, updated_at)
             VALUES ('scratch', 'Scratch', 'fixture article', x'01020304', '2026-01-01', '2026-01-01');",
        )
        .expect("wal row");
    let environment = support::environment(&home);
    let real = memory(
        &[
            "reembed",
            "--dry-run",
            "--sqlite",
            &path.display().to_string(),
        ],
        &environment,
    )
    .await;
    let linked = memory(
        &[
            "reembed",
            "--dry-run",
            "--sqlite",
            &alias.display().to_string(),
        ],
        &environment,
    )
    .await;
    assert_eq!(real.exit, Exit::Ok, "{}", real.stderr);
    assert_eq!(linked.exit, Exit::Ok, "{}", linked.stderr);
    assert_eq!(
        real.stdout,
        "would clear 1 stored vector(s) and queue 1 embed job(s)\n"
    );
    assert_eq!(real.stdout, linked.stdout);
    drop(writer);
}

#[tokio::test]
async fn reembed_and_import_dry_runs_open_the_live_file() {
    let home = support::home();
    let environment = support::environment(&home);
    let reembed_path = home.path().join("reembed.sqlite");
    let import_path = home.path().join("import.sqlite");
    migrate::sqlite(&reembed_path).expect("schema");
    migrate::sqlite(&import_path).expect("schema");
    force_delete_journal(&reembed_path);
    force_delete_journal(&import_path);
    let dump = testkit::recording("memory-sqlite/import/copy-cases.txt");
    let reembed = memory(
        &[
            "reembed",
            "--dry-run",
            "--sqlite",
            &reembed_path.display().to_string(),
        ],
        &environment,
    )
    .await;
    let import = memory(
        &[
            "import",
            &dump.display().to_string(),
            "--dry-run",
            "--sqlite",
            &import_path.display().to_string(),
        ],
        &environment,
    )
    .await;
    assert_eq!(reembed.exit, Exit::Ok, "{}", reembed.stderr);
    assert_eq!(import.exit, Exit::Ok, "{}", import.stderr);
    assert_eq!(journal_mode(&reembed_path), "wal");
    assert_eq!(journal_mode(&import_path), "wal");
}

#[tokio::test]
async fn settle_resolves_an_explicit_sqlite_file_before_config() {
    let home = support::home();
    let path = home.path().join("scratch.sqlite");
    migrate::sqlite(&path).expect("schema");
    let environment = missing_config(&home);
    let report = memory(
        &[
            "settle",
            "scratch-session",
            "--sqlite",
            &path.display().to_string(),
        ],
        &environment,
    )
    .await;
    assert!(
        report.stderr.contains("scratch-session"),
        "{}",
        report.stderr
    );
    assert!(
        !report.stderr.contains("failed to read config"),
        "{}",
        report.stderr
    );
}

#[tokio::test]
async fn explicit_sqlite_commands_do_not_read_a_missing_config() {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database).expect("schema");
    let path = database.display().to_string();
    let out = home.path().join("out").display().to_string();
    let dump = testkit::recording("memory-sqlite/import/copy-cases.txt");
    let dump = dump.display().to_string();
    let session = "00000000-0000-4000-8000-00000000000a";
    let environment = missing_config(&home);
    let mut blocked = String::new();
    for (name, args) in sqlite_commands(&path, &out, &dump, session) {
        let report = memory(&args, &environment).await;
        if report.stderr.contains("failed to read config") {
            blocked.push_str(name);
            blocked.push('\n');
        }
    }
    assert_eq!(blocked, "");
}

fn missing_config(home: &tempfile::TempDir) -> config::Environment {
    support::environment(home).with_override(
        "RIVETHUB_CONFIG",
        home.path().join("missing.yaml").display().to_string(),
    )
}

fn sqlite_commands<'a>(
    path: &'a str,
    out: &'a str,
    dump: &'a str,
    session: &'a str,
) -> Vec<(&'a str, Vec<&'a str>)> {
    vec![
        ("queue-status", vec!["queue-status", "--sqlite", path]),
        ("pause", vec!["pause", "--sqlite", path]),
        ("resume", vec!["resume", "wiki-builder", "--sqlite", path]),
        (
            "retry-failed",
            vec![
                "retry-failed",
                "--task",
                "extract-wiki",
                "--dry-run",
                "--sqlite",
                path,
            ],
        ),
        (
            "requeue",
            vec![
                "requeue",
                "--task",
                "extract-wiki",
                "--dry-run",
                "--sqlite",
                path,
            ],
        ),
        ("export", vec!["export", "--sqlite", path]),
        ("bundle", vec!["bundle", "--out", out, "--sqlite", path]),
        (
            "sanitize",
            vec![
                "sanitize",
                "--session",
                session,
                "--out",
                out,
                "--sqlite",
                path,
            ],
        ),
        (
            "import",
            vec!["import", dump, "--dry-run", "--sqlite", path],
        ),
        (
            "backfill-titles",
            vec!["backfill-titles", "--dry-run", "--sqlite", path],
        ),
        ("reembed", vec!["reembed", "--dry-run", "--sqlite", path]),
        (
            "consolidate-wiki",
            vec!["consolidate-wiki", "--dry-run", "--sqlite", path],
        ),
        (
            "resummarize",
            vec!["resummarize", "status", "--sqlite", path],
        ),
    ]
    .into_iter()
    .chain(cleanup_commands(path, out))
    .collect()
}

fn cleanup_commands<'a>(path: &'a str, out: &'a str) -> [(&'a str, Vec<&'a str>); 2] {
    [
        (
            "cleanup",
            vec![
                "cleanup",
                "--dry-run",
                "--sqlite",
                path,
                "--out",
                out,
                "--model-url",
                "http://127.0.0.1:1/v1",
                "--model",
                "unused",
                "--timeout",
                "1",
            ],
        ),
        (
            "cleanup-propose",
            vec![
                "cleanup",
                "--propose",
                "--sqlite",
                path,
                "--out",
                out,
                "--model-url",
                "http://127.0.0.1:1/v1",
                "--model",
                "unused",
            ],
        ),
    ]
}

async fn memory(parts: &[&str], environment: &config::Environment) -> cli::Report {
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(&args, environment, &CancellationToken::new(), false).await
}

fn force_delete_journal(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    let mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("journal");
    assert_eq!(mode, "delete");
}

fn journal_mode(path: &std::path::Path) -> String {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cleanup_refuses_checkpoints_after_the_database_is_recreated(pool: sqlx::PgPool) {
    let name = scratch_name(&pool).await;
    let url = database_url(&pool, &name);
    create_database(&pool, &name).await;
    let home = support::home();
    let environment = support::environment(&home);
    let out = home.path().join("cleanup");
    let first = cleanup_database(&environment, &url, &out).await;
    let second = cleanup_database(&environment, &url, &out).await;
    drop_database(&pool, &name).await;
    create_database(&pool, &name).await;
    let recreated = cleanup_database(&environment, &url, &out).await;
    drop_database(&pool, &name).await;
    assert!(out.join("target.json").is_file(), "{}", first.stderr);
    assert!(
        !second.stderr.contains("different database"),
        "{}",
        second.stderr
    );
    assert!(
        recreated.stderr.contains("different database"),
        "{}",
        recreated.stderr
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cleanup_uses_an_explicit_database_before_a_missing_config(pool: sqlx::PgPool) {
    let url = pool.connect_options().as_ref().to_url_lossy();
    let home = support::home();
    let environment = missing_config(&home);
    let out = home.path().join("cleanup");
    let report = cleanup_database(&environment, url.as_ref(), &out).await;
    assert!(out.join("target.json").is_file(), "{}", report.stderr);
    assert!(
        !report.stderr.contains("failed to read config"),
        "{}",
        report.stderr
    );
    assert!(
        !report.stderr.contains("No config found"),
        "{}",
        report.stderr
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cleanup_accepts_a_postgres_url_whose_host_is_in_the_query(pool: sqlx::PgPool) {
    let via = host_in_query(&pool.connect_options().as_ref().to_url_lossy());
    let home = support::home();
    let environment = support::environment(&home);
    let out = home.path().join("query");
    let report = cleanup_database(&environment, &via, &out).await;
    assert!(out.join("target.json").is_file(), "{}", report.stderr);
    assert!(!report.stderr.contains("has no host"), "{}", report.stderr);
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cleanup_refuses_an_origin_stamp_and_an_unstamped_directory(pool: sqlx::PgPool) {
    let url = pool.connect_options().as_ref().to_url_lossy();
    let name = url.path().trim_start_matches('/').to_owned();
    let home = support::home();
    let environment = support::environment(&home);
    let out = home.path().join("cleanup");
    let first = cleanup_database(&environment, url.as_ref(), &out).await;
    assert!(out.join("target.json").is_file(), "{}", first.stderr);
    let text = std::fs::read_to_string(out.join("target.json")).expect("stamp");
    std::fs::write(out.join("target.json"), legacy_stamp(&text, &name)).expect("legacy");
    std::fs::write(out.join("snapshot.json"), "checkpoint").expect("snapshot");
    let stamped = cleanup_database(&environment, url.as_ref(), &out).await;
    std::fs::remove_file(out.join("target.json")).expect("remove stamp");
    let unstamped = cleanup_database(&environment, url.as_ref(), &out).await;
    assert!(
        stamped.stderr.contains("different database"),
        "{}",
        stamped.stderr
    );
    assert!(
        unstamped.stderr.contains("no record of which database"),
        "{}",
        unstamped.stderr
    );
}

async fn scratch_name(pool: &sqlx::PgPool) -> String {
    sqlx::query_scalar("SELECT 'rhc' || left(md5(current_database() || 'stamp'), 16)")
        .fetch_one(pool)
        .await
        .expect("name")
}

fn database_url(pool: &sqlx::PgPool, name: &str) -> String {
    let mut url = pool.connect_options().as_ref().to_url_lossy();
    url.set_path(&format!("/{name}"));
    url.to_string()
}

fn host_in_query(direct: &url::Url) -> String {
    let database = direct.path().trim_start_matches('/');
    let mut via = url::Url::parse(&format!("postgresql:///{database}")).expect("url");
    let port = direct.port().expect("port").to_string();
    let mut pairs = via.query_pairs_mut();
    pairs
        .append_pair("host", direct.host_str().expect("host"))
        .append_pair("port", &port)
        .append_pair("user", direct.username());
    if let Some(password) = direct.password() {
        pairs.append_pair("password", password);
    }
    drop(pairs);
    via.to_string()
}

fn legacy_stamp(text: &str, name: &str) -> String {
    if text.contains("\"host\"") {
        text.to_owned()
    } else {
        format!(r#"{{"kind":"postgres","source":"local","host":"localhost","database":"{name}"}}"#)
    }
}

async fn create_database(pool: &sqlx::PgPool, name: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name}"
    )))
    .execute(pool)
    .await
    .expect("drop before create");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(pool)
        .await
        .expect("create");
}

async fn drop_database(pool: &sqlx::PgPool, name: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name}"
    )))
    .execute(pool)
    .await
    .expect("drop");
}

#[tokio::test]
async fn a_held_write_lock_says_the_store_is_busy() {
    let home = support::home();
    let path = home.path().join("queue.sqlite");
    migrate::sqlite(&path).expect("schema");
    let writer = rusqlite::Connection::open(&path).expect("writer");
    writer.execute_batch("BEGIN IMMEDIATE").expect("lock");
    let environment = support::environment(&home);
    let report = memory(
        &[
            "retry-failed",
            "--task",
            "extract-wiki",
            "--sqlite",
            &path.display().to_string(),
        ],
        &environment,
    )
    .await;
    writer.execute_batch("ROLLBACK").expect("unlock");
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        "Error: the store is busy; run this command again\n"
    );
}

async fn cleanup_database(
    environment: &config::Environment,
    database: &str,
    out: &std::path::Path,
) -> cli::Report {
    let out = out.display().to_string();
    let parts = [
        "cleanup",
        "--dry-run",
        "--database",
        database,
        "--out",
        out.as_str(),
        "--model-url",
        "http://127.0.0.1:1/v1",
        "--model",
        "unused",
        "--timeout",
        "1",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(&args, environment, &CancellationToken::new(), false).await
}
