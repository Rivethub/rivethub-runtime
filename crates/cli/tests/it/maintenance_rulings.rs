#![cfg(test)]

use crate::support;

use cli::Exit;
use sqlx::ConnectOptions as _;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn propose_refuses_a_relative_live_file_from_the_service_home() {
    let home = support::home();
    let relative = home
        .path()
        .file_name()
        .expect("tempdir name")
        .to_str()
        .expect("utf-8 name")
        .to_owned();
    let live = home.path().join(&relative);
    migrate::sqlite(&live).expect("schema");
    let out = home.path().join("blocked-output");
    std::fs::write(&out, "regular file").expect("out file");
    support::config(
        &home,
        &format!("memory:\n  backend: sqlite\n  sqlite:\n    path: {relative}\n"),
    );
    let database = live.to_str().expect("database path");
    let output = out.to_str().expect("out path");
    let report = memory(
        &[
            "cleanup",
            "--propose",
            "--sqlite",
            database,
            "--out",
            output,
            "--model-url",
            "http://127.0.0.1:1/v1",
            "--model",
            "unused",
        ],
        &support::environment(&home),
    )
    .await;
    assert!(
        report
            .stderr
            .contains("refusing to write cleanup proposals"),
        "{}",
        report.stderr
    );
}

#[tokio::test]
async fn propose_refuses_when_the_configured_sqlite_file_is_missing() {
    let home = support::home();
    let missing = home.path().join("missing.sqlite");
    let other = home.path().join("other.sqlite");
    migrate::sqlite(&other).expect("schema");
    let out = home.path().join("proposals");
    std::fs::create_dir(&out).expect("out directory");
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            missing.display()
        ),
    );
    let database = other.to_str().expect("database path");
    let output = out.to_str().expect("out path");
    let report = memory(
        &[
            "cleanup",
            "--propose",
            "--sqlite",
            database,
            "--out",
            output,
            "--model-url",
            "http://127.0.0.1:1/v1",
            "--model",
            "unused",
        ],
        &support::environment(&home),
    )
    .await;
    assert!(
        report.stderr.contains("cannot be identified"),
        "{}",
        report.stderr
    );
    assert_eq!(std::fs::read_dir(&out).expect("out").count(), 0);
}

#[tokio::test]
async fn explicit_sqlite_does_not_need_a_config_for_queue_status() {
    let home = support::home();
    let config = home.path().join("missing.yaml");
    let path = home.path().join("scratch.sqlite");
    migrate::sqlite(&path).expect("schema");
    let environment = support::environment(&home)
        .with_override("RIVETHUB_CONFIG", config.to_str().expect("config path"));
    let database = path.to_str().expect("database path");
    let report = memory(&["queue-status", "--sqlite", database], &environment).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
}

const CLOSED: &str = "postgres://127.0.0.1:1/closed";
const CONNECTION_FAILED: &str = "connection to the memory database failed";
const DATAHUB: &str = "This command needs a datahub (Postgres).";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";

struct Fixture {
    dump: String,
    bundle: String,
    sanitize: String,
    cleanup: String,
    wiki: String,
    missing: String,
    from: String,
}

impl Fixture {
    fn prepare(home: &std::path::Path, from: &str) -> Self {
        let dump = home.join("dump.sql");
        std::fs::write(&dump, "-- source local\n-- host fixture\n").expect("dump");
        let wiki = home.join("wiki");
        std::fs::create_dir_all(wiki.join("topics")).expect("topics");
        std::fs::write(wiki.join("topics/acme.md"), "# acme\n").expect("page");
        Self {
            dump: utf8(&dump),
            bundle: utf8(&home.join("bundle.json")),
            sanitize: utf8(&home.join("sanitized.json")),
            cleanup: utf8(&home.join("cleanup-out")),
            wiki: utf8(&wiki),
            missing: utf8(&home.join("missing-bundle.json")),
            from: from.to_owned(),
        }
    }
}

fn utf8(path: &std::path::Path) -> String {
    path.to_str().expect("utf-8 path").to_owned()
}

fn named(name: &'static str, parts: &[&str]) -> (&'static str, Vec<String>) {
    (name, parts.iter().map(|part| (*part).to_owned()).collect())
}

fn maintenance_commands(fixture: &Fixture) -> Vec<(&'static str, Vec<String>)> {
    vec![
        named("settle", &["settle", "missing-session"]),
        named("resummarize", &["resummarize", "status"]),
        named("pause", &["pause"]),
        named("resume", &["resume", "categorizer"]),
        named("queue-status", &["queue-status"]),
        named(
            "retry-failed",
            &["retry-failed", "--task", "extract-wiki", "--dry-run"],
        ),
        named(
            "requeue",
            &["requeue", "--task", "extract-wiki", "--dry-run"],
        ),
        named("backfill-tool-synth", &["backfill-tool-synth", "--dry-run"]),
        named("export", &["export"]),
        named("import", &["import", &fixture.dump, "--dry-run"]),
        named("bundle", &["bundle", "--out", &fixture.bundle]),
        named(
            "sanitize",
            &["sanitize", "--session", SESSION, "--out", &fixture.sanitize],
        ),
        named("backfill-titles", &["backfill-titles", "--dry-run"]),
        named(
            "backfill-titles-model",
            &[
                "backfill-titles",
                "--dry-run",
                "--model",
                "m",
                "--model-url",
                "http://127.0.0.1:1/v1",
            ],
        ),
        named("consolidate-wiki", &["consolidate-wiki", "--dry-run"]),
        named(
            "cleanup",
            &[
                "cleanup",
                "--dry-run",
                "--out",
                &fixture.cleanup,
                "--model-url",
                "http://127.0.0.1:1/v1",
                "--model",
                "m",
                "--timeout",
                "1",
            ],
        ),
        named(
            "recompile",
            &[
                "recompile-wiki",
                "--since",
                "2026-01-01T00:00:00Z",
                "--slug",
                "acme",
                "--min-bytes",
                "1",
            ],
        ),
        named(
            "merge-store",
            &[
                "merge-store",
                "--from",
                &fixture.from,
                "--owner",
                "owner",
                "--dry-run",
            ],
        ),
    ]
}

fn postgres_only(name: &str) -> bool {
    name == "backfill-titles-model" || name == "merge-store"
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_environment_url_beats_a_configured_url_for_maintenance_commands(pool: sqlx::PgPool) {
    let live = pool.connect_options().as_ref().to_url_lossy().to_string();
    let home = support::home();
    let fixture = Fixture::prepare(home.path(), &live);
    support::config(
        &home,
        &format!("memory:\n  postgres:\n    connection_string: \"{live}\"\n"),
    );
    let environment = support::environment(&home)
        .with_override("RIVETHUB_PG_URL", CLOSED)
        .with_override("WIKI_DIR", &fixture.wiki);
    let mut missed = String::new();
    for (name, args) in maintenance_commands(&fixture) {
        let report = memory_args(&args, &environment).await;
        if !report.stderr.contains(CONNECTION_FAILED) {
            missed.push_str(name);
            missed.push('\n');
            missed.push_str(&report.stderr);
            missed.push_str(&report.stdout);
            missed.push('\n');
        }
    }
    assert_eq!(missed, "");
    let share = memory(&["share", &fixture.missing], &environment).await;
    assert_eq!(share.stdout, "");
    assert!(
        share.stderr.contains("No such file or directory"),
        "{}",
        share.stderr
    );
    assert!(
        !share.stderr.contains(CONNECTION_FAILED),
        "{}",
        share.stderr
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn each_maintenance_command_uses_the_only_configured_url(pool: sqlx::PgPool) {
    let live = pool.connect_options().as_ref().to_url_lossy().to_string();
    let home = support::home();
    let fixture = Fixture::prepare(home.path(), &live);
    support::config(
        &home,
        &format!("memory:\n  postgres:\n    connection_string: \"{CLOSED}\"\n"),
    );
    let environment = support::environment(&home).with_override("WIKI_DIR", &fixture.wiki);
    expect_closed(&fixture, &environment).await;
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn each_maintenance_command_uses_the_only_environment_url(pool: sqlx::PgPool) {
    let live = pool.connect_options().as_ref().to_url_lossy().to_string();
    let home = support::home();
    let fixture = Fixture::prepare(home.path(), &live);
    support::config(&home, "memory:\n  backend: postgres\n");
    let environment = support::environment(&home)
        .with_override("RIVETHUB_PG_URL", CLOSED)
        .with_override("WIKI_DIR", &fixture.wiki);
    expect_closed(&fixture, &environment).await;
}

#[tokio::test]
async fn a_sqlite_node_ignores_an_environment_url_for_maintenance_commands() {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database).expect("schema");
    let fixture = Fixture::prepare(home.path(), CLOSED);
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            database.display()
        ),
    );
    let environment = support::environment(&home)
        .with_override("RIVETHUB_PG_URL", CLOSED)
        .with_override("WIKI_DIR", &fixture.wiki);
    for (name, args) in maintenance_commands(&fixture) {
        let report = memory_args(&args, &environment).await;
        assert!(
            !report.stderr.contains(CONNECTION_FAILED),
            "{name}: {}",
            report.stderr
        );
        if matches!(name, "queue-status" | "backfill-tool-synth") {
            assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
        }
        if name == "recompile" {
            assert!(
                report.stderr.contains("needs Postgres in this version"),
                "{name}: {}",
                report.stderr
            );
        }
        if postgres_only(name) {
            assert!(report.stderr.contains(DATAHUB), "{name}: {}", report.stderr);
        }
    }
}

async fn expect_closed(fixture: &Fixture, environment: &config::Environment) {
    let mut missed = String::new();
    for (name, args) in maintenance_commands(fixture) {
        let report = memory_args(&args, environment).await;
        if !report.stderr.contains(CONNECTION_FAILED) {
            missed.push_str(name);
            missed.push('\n');
        }
    }
    assert_eq!(missed, "");
}

#[tokio::test]
async fn dry_runs_leave_a_delete_journal_unchanged() {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database).expect("schema");
    force_delete_journal(&database);
    let path = database.display().to_string();
    let out = home.path().join("cleanup-out");
    let out = out.display().to_string();
    let environment = support::environment(&home);
    let mut changed = String::new();
    for (name, args) in dry_reads(&path, &out) {
        let report = memory_args(&args, &environment).await;
        if name != "cleanup" {
            assert_eq!(report.exit, Exit::Ok, "{name}: {}", report.stderr);
        }
        let mode = journal_mode(&database);
        if mode != "delete" {
            changed.push_str(name);
            changed.push('=');
            changed.push_str(&mode);
            changed.push('\n');
            force_delete_journal(&database);
        }
    }
    let snapshot = home.path().join("cleanup-out").join("snapshot.json");
    assert!(snapshot.is_file());
    let help = memory_args(&[], &environment).await;
    if !help.stdout.contains("do not change its journal mode") {
        changed.push_str("help\n");
    }
    assert_eq!(changed, "");
}

fn dry_reads(path: &str, out: &str) -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("queue-status", words(&["queue-status", "--sqlite", path])),
        (
            "retry-failed",
            words(&[
                "retry-failed",
                "--task",
                "extract-wiki",
                "--dry-run",
                "--sqlite",
                path,
            ]),
        ),
        (
            "requeue",
            words(&[
                "requeue",
                "--task",
                "extract-wiki",
                "--dry-run",
                "--sqlite",
                path,
            ]),
        ),
        (
            "consolidate-wiki",
            words(&["consolidate-wiki", "--dry-run", "--sqlite", path]),
        ),
        (
            "backfill-titles",
            words(&["backfill-titles", "--dry-run", "--sqlite", path]),
        ),
        (
            "cleanup",
            words(&[
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
            ]),
        ),
    ]
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn force_delete_journal(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    let mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("journal");
    assert_eq!(mode, "delete");
    drop(connection);
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar(path, suffix);
        if sidecar.is_file() {
            std::fs::remove_file(sidecar).expect("sidecar");
        }
    }
}

fn journal_mode(path: &std::path::Path) -> String {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal")
}

fn sqlite_sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.file_name().expect("file name").to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

#[tokio::test]
async fn recompile_on_a_sqlite_node_fails_before_scanning() {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            database.display()
        ),
    );
    let wiki = home.path().join("wiki");
    std::fs::create_dir_all(wiki.join("topics")).expect("wiki");
    let environment = support::environment(&home)
        .with_override("WIKI_DIR", wiki.display().to_string())
        .with_override("RIVETHUB_PG_URL", "postgres://127.0.0.1:1/closed");
    let mut missed = String::new();
    note_recompile(&mut missed, "dry-run", &recompile(&environment, true).await);
    std::fs::remove_dir_all(&wiki).expect("remove wiki");
    note_recompile(
        &mut missed,
        "missing-wiki",
        &recompile(&environment, true).await,
    );
    std::fs::create_dir_all(wiki.join("topics")).expect("wiki");
    std::fs::write(wiki.join("topics").join("acme.md"), "# Acme\n").expect("page");
    let enqueue = recompile(&environment, false).await;
    note_recompile(&mut missed, "enqueue", &enqueue);
    if enqueue
        .stderr
        .contains("connection to the memory database failed")
    {
        missed.push_str("enqueue-connected\n");
    }
    assert_eq!(missed, "");
}

fn note_recompile(missed: &mut String, name: &str, report: &cli::Report) {
    if report.exit != Exit::Failure || !report.stderr.contains("needs Postgres in this version") {
        missed.push_str(name);
        missed.push('\n');
        missed.push_str(&report.stderr);
        missed.push_str(&report.stdout);
        missed.push('\n');
    }
}

async fn recompile(environment: &config::Environment, dry_run: bool) -> cli::Report {
    let mut args = vec![
        "recompile-wiki".to_owned(),
        "--since".to_owned(),
        "2026-01-01T00:00:00Z".to_owned(),
        "--slug".to_owned(),
        "acme".to_owned(),
        "--min-bytes".to_owned(),
        "1".to_owned(),
    ];
    if dry_run {
        args.push("--dry-run".to_owned());
        args.push("--json".to_owned());
    }
    memory_args(&args, environment).await
}

async fn memory(parts: &[&str], environment: &config::Environment) -> cli::Report {
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    memory_args(&args, environment).await
}

async fn memory_args(args: &[String], environment: &config::Environment) -> cli::Report {
    cli::memory::execute(args, environment, &CancellationToken::new(), false).await
}
