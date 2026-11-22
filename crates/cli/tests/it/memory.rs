#![cfg(test)]

use crate::support;

use std::path::{Path, PathBuf};

use cli::{Exit, Report};
use config::Environment;
use serde::Deserialize;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const SESSION: &str = "round-trip-session-key";
const BODY: &str = "round-trip-body";

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn env(pairs: &[(&str, &str)]) -> (TempDir, Environment) {
    let home = support::home();
    let mut environment = support::environment(&home);
    for (key, value) in pairs {
        environment = environment.with_override(key, value);
    }
    (home, environment)
}

async fn memory(parts: &[&str], environment: &Environment, tty: bool) -> Report {
    cli::memory::execute(&args(parts), environment, &CancellationToken::new(), tty).await
}

async fn run(parts: &[&str], environment: &Environment) -> Report {
    support::run(parts, environment).await
}

fn failure(report: &Report, text: &str) {
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(report.stdout, "");
    assert_eq!(report.stderr, format!("Error: {text}\n"));
}

#[tokio::test]
async fn memory_help_and_unknown_words_exit_clean() {
    let (_home, environment) = env(&[]);
    for parts in [
        vec!["memory"],
        vec!["memory", "--help"],
        vec!["memory", "frob"],
        vec!["memory", "tags"],
        vec!["memory", "tags", "frob"],
        vec!["memory", "export", "--help"],
    ] {
        let report = run(&parts, &environment).await;
        assert_eq!(report.exit, Exit::Ok, "{parts:?}");
        assert_eq!(report.stderr, "");
        assert!(report.stdout.contains("queue-status"));
        assert!(
            report
                .stdout
                .contains(wire::gateway::memory::SEARCH_QUERY_GUIDANCE)
        );
        assert!(!report.stdout.contains("doctor"));
    }
}

#[tokio::test]
async fn memory_rejects_unknown_options_and_incomplete_flags() {
    let (_home, environment) = env(&[]);
    let missing = run(&["memory", "queue-status"], &environment).await;
    failure(&missing, "This command needs a datahub (Postgres).");
    let unknown = run(&["memory", "queue-status", "--sqlite", "db"], &environment).await;
    failure(&unknown, "Unknown option: --sqlite");
    let retry = run(&["memory", "retry-failed"], &environment).await;
    failure(
        &retry,
        "At least one --task <identifier> is required (e.g. --task extract-wiki). Refusing to reset every dead job without an explicit task filter.",
    );
    let requeue = run(&["memory", "requeue", "--task", "nope"], &environment).await;
    failure(
        &requeue,
        "--task nope is not an allowed memory task (extract-wiki, compact-conversation, embed-target, synthesize-tool-call, file-conversation)",
    );
    failure(
        &run(
            &[
                "memory",
                "requeue",
                "--task",
                "extract-wiki",
                "--task",
                "embed-target",
            ],
            &environment,
        )
        .await,
        "requeue accepts one --task",
    );
    failure(
        &run(&["memory", "requeue", "--limit", "0"], &environment).await,
        "--limit requires a positive integer",
    );
    failure(
        &run(
            &["memory", "backfill-tool-synth", "--limit", "abc"],
            &environment,
        )
        .await,
        "--limit requires an integer",
    );
    failure(
        &run(&["memory", "import"], &environment).await,
        "import requires a file path",
    );
    failure(
        &run(&["memory", "import", "a", "b"], &environment).await,
        "import accepts one file path",
    );
    failure(
        &run(&["memory", "export", "--since", "yesterday"], &environment).await,
        "invalid --since timestamp: yesterday",
    );
    failure(
        &run(&["memory", "export", "--sqlite"], &environment).await,
        "--sqlite requires a file path",
    );
}

#[tokio::test]
async fn sqlite_config_does_not_satisfy_a_queue_command() {
    let (home, environment) = env(&[]);
    support::config(&home, "memory:\n  sqlite:\n    path: /tmp/memory.sqlite\n");
    let report = run(&["memory", "queue-status"], &environment).await;
    failure(&report, "This command needs a datahub (Postgres).");
    assert!(
        !report.stderr.contains("RIVETHUB_PG_URL"),
        "{}",
        report.stderr
    );
}

#[tokio::test]
async fn a_sqlite_node_keeps_postgres_commands_off_a_stray_database_url() {
    let (home, environment) = env(&[("RIVETHUB_PG_URL", "postgres://127.0.0.1:1/stray")]);
    let database = home.path().join("memory.sqlite");
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            database.display()
        ),
    );
    for command in ["queue-status", "consolidate-wiki", "backfill-tool-synth"] {
        let report = run(&["memory", command], &environment).await;
        failure(&report, "This command needs a datahub (Postgres).");
        assert!(
            !report.stderr.contains("RIVETHUB_PG_URL"),
            "{command}: {}",
            report.stderr
        );
    }
    let out = home.path().join("unused-export.dump").display().to_string();
    let exported = run(&["memory", "export", "--out", &out], &environment).await;
    assert_eq!(exported.exit, Exit::Failure, "{}", exported.stderr);
    assert!(
        exported.stderr.contains("is not initialised"),
        "{}",
        exported.stderr
    );
    assert!(
        !exported.stderr.contains("RIVETHUB_PG_URL"),
        "{}",
        exported.stderr
    );
}

#[tokio::test]
async fn sqlite_import_wins_over_a_postgres_url() {
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", "postgres://user:secret@db.example/db")]);
    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    let imported = memory(
        &[
            "import",
            &recording(),
            "--sqlite",
            &database.to_string_lossy(),
        ],
        &environment,
        false,
    )
    .await;
    assert_eq!(imported.exit, Exit::Ok, "{}", imported.stderr);
    assert_eq!(scalar(&database, "SELECT count(*) FROM ros_summaries"), 2);
}

#[tokio::test]
async fn sqlite_reembed_clears_imported_vectors_and_requeues_every_row() {
    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    let path = database.to_string_lossy().to_string();
    let (_home, environment) = env(&[]);
    let imported = run(
        &["memory", "import", &recording(), "--sqlite", &path],
        &environment,
    )
    .await;
    assert_eq!(imported.exit, Exit::Ok, "{}", imported.stderr);
    let preview = run(
        &["memory", "reembed", "--sqlite", &path, "--dry-run"],
        &environment,
    )
    .await;
    assert_eq!(preview.exit, Exit::Ok, "{}", preview.stderr);
    assert_eq!(
        preview.stdout,
        "would clear 3 stored vector(s) and queue 4 embed job(s)\n"
    );
    assert_eq!(vectors(&database), 3);
    let applied = run(&["memory", "reembed", "--sqlite", &path], &environment).await;
    assert_eq!(applied.exit, Exit::Ok, "{}", applied.stderr);
    assert_eq!(
        applied.stdout,
        "cleared 3 stored vector(s) and queued 4 embed job(s); the next embedding records the configured model\n"
    );
    assert_eq!(vectors(&database), 0);
    assert_eq!(
        scalar(
            &database,
            "SELECT count(*) FROM ros_jobs WHERE task = 'embed-target' AND state = 'queued'"
        ),
        4
    );
    assert_eq!(
        scalar(
            &database,
            "SELECT count(*) FROM ros_meta WHERE key IN ('embed_model', 'embed_dims')"
        ),
        0
    );
    let (_home, postgres) = env(&[("RIVETHUB_PG_URL", "postgres://127.0.0.1/memory")]);
    failure(
        &run(&["memory", "reembed"], &postgres).await,
        "reembed runs on a SQLite memory file; pass --sqlite <file>",
    );
}

fn vectors(path: &Path) -> i64 {
    scalar(
        path,
        "SELECT (SELECT count(*) FROM ros_messages WHERE embedding IS NOT NULL) \
              + (SELECT count(*) FROM ros_summaries WHERE embedding IS NOT NULL) \
              + (SELECT count(*) FROM ros_wiki_topics WHERE embedding IS NOT NULL)",
    )
}

fn recording() -> String {
    testkit::recording("memory-sqlite/import/copy-cases.txt")
        .display()
        .to_string()
}

fn migrated(directory: &Path) -> PathBuf {
    let path = directory.join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    path
}

fn scalar(path: &Path, sql: &str) -> i64 {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    connection
        .query_row(sql, [], |row| row.get(0))
        .expect("scalar")
}

fn gunzip(bytes: &[u8]) -> String {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(bytes);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gzip");
    text
}

#[tokio::test]
async fn sqlite_import_dry_run_writes_nothing_and_a_real_run_imports() {
    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    let (_home, environment) = env(&[]);
    let path = database.to_string_lossy().to_string();
    let preview = run(
        &[
            "memory",
            "import",
            &recording(),
            "--sqlite",
            &path,
            "--dry-run",
        ],
        &environment,
    )
    .await;
    assert_eq!(preview.exit, Exit::Ok, "{}", preview.stderr);
    let summary: ImportSummary = serde_json::from_str(preview.stdout.trim()).expect("summary");
    assert!(summary.dry_run);
    assert_eq!(summary.rows.get("summaries"), Some(&2));
    assert_eq!(scalar(&database, "SELECT count(*) FROM ros_summaries"), 0);
    let applied = run(
        &["memory", "import", &recording(), "--sqlite", &path],
        &environment,
    )
    .await;
    assert_eq!(applied.exit, Exit::Ok, "{}", applied.stderr);
    assert_eq!(scalar(&database, "SELECT count(*) FROM ros_summaries"), 2);
    assert_eq!(
        scalar(&database, "SELECT count(*) FROM ros_conversations"),
        1
    );
}

#[tokio::test]
async fn sqlite_gzip_round_trip_filters_future_rows_and_guards_the_output() {
    let directory = tempfile::tempdir().expect("directory");
    let source = migrated(directory.path());
    let destination = directory.path().join("copy.sqlite");
    migrate::sqlite(&destination).expect("migrate");
    let (_home, environment) = env(&[]);
    let source_text = source.to_string_lossy().to_string();
    assert_eq!(
        run(
            &["memory", "import", &recording(), "--sqlite", &source_text],
            &environment
        )
        .await
        .exit,
        Exit::Ok
    );
    let out = directory.path().join("memory.dump.gz");
    let out_text = out.display().to_string();
    let wrote = run(
        &[
            "memory",
            "export",
            "--sqlite",
            &source_text,
            "--out",
            &out_text,
        ],
        &environment,
    )
    .await;
    assert_eq!(wrote.stdout, format!("wrote {out_text}\n"));
    assert_eq!(std::fs::read(&out).expect("gzip").first(), Some(&0x1f));
    let plain = gunzip(&std::fs::read(&out).expect("gzip"));
    assert!(plain.contains("import-session"));
    assert!(plain.contains("-- PostgreSQL database dump complete"));
    let future = directory.path().join("future.dump.gz");
    let future_text = future.display().to_string();
    assert_eq!(
        run(
            &[
                "memory",
                "export",
                "--sqlite",
                &source_text,
                "--since",
                "2099-01-01T00:00:00Z",
                "--out",
                &future_text
            ],
            &environment,
        )
        .await
        .exit,
        Exit::Ok
    );
    assert!(!gunzip(&std::fs::read(&future).expect("gzip")).contains("import-session"));
    let copied = run(
        &[
            "memory",
            "import",
            &out_text,
            "--sqlite",
            &destination.to_string_lossy(),
        ],
        &environment,
    )
    .await;
    assert_eq!(copied.exit, Exit::Ok, "{}", copied.stderr);
    assert_eq!(
        scalar(&destination, "SELECT count(*) FROM ros_summaries"),
        2
    );
    export_output_guards(directory.path(), &environment, &source_text, &out).await;
}

async fn export_output_guards(
    directory: &Path,
    environment: &Environment,
    source_text: &str,
    out: &Path,
) {
    let captured = memory(&["export", "--sqlite", source_text], environment, false).await;
    assert_eq!(captured.stdout, "");
    assert_eq!(captured.stdout_raw.first(), Some(&0x1f));
    let tty = memory(&["export", "--sqlite", source_text], environment, true).await;
    failure(
        &tty,
        "refusing to write gzip to a TTY (redirect stdout or pass --out <file>)",
    );
    let missing = directory.join("missing").join("out.gz");
    let refused = run(
        &[
            "memory",
            "export",
            "--sqlite",
            source_text,
            "--out",
            &missing.to_string_lossy(),
        ],
        environment,
    )
    .await;
    failure(&refused, "output directory does not exist");
    let link = directory.join("link.gz");
    std::os::unix::fs::symlink(out, &link).expect("symlink");
    let linked = run(
        &[
            "memory",
            "export",
            "--sqlite",
            source_text,
            "--out",
            &link.to_string_lossy(),
        ],
        environment,
    )
    .await;
    failure(&linked, "output path is a symlink");
}

#[derive(Debug, Deserialize)]
struct ImportSummary {
    rows: std::collections::BTreeMap<String, u64>,
    dry_run: bool,
}

async fn install_graphile(pool: &PgPool) {
    let present: bool = sqlx::query_scalar("SELECT to_regnamespace('graphile_worker') IS NOT NULL")
        .fetch_one(pool)
        .await
        .expect("queue schema lookup");
    if present {
        return;
    }
    let mut transaction = pool.begin().await.expect("begin");
    let path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut *transaction)
        .await
        .expect("search path");
    sqlx::raw_sql(include_str!(
        "../../../rivethub/tests/fixtures/graphile_worker.txt"
    ))
    .execute(&mut *transaction)
    .await
    .expect("graphile schema");
    sqlx::query("SELECT set_config('search_path', $1, false)")
        .bind(path)
        .execute(&mut *transaction)
        .await
        .expect("restore search path");
    transaction.commit().await.expect("commit");
}

fn url_of(pool: &PgPool) -> String {
    pool.connect_options().as_ref().to_url_lossy().to_string()
}

async fn dead_job(
    pool: &PgPool,
    task: &str,
    key: &str,
    error: &str,
    idle_hours: i32,
    lock_hours: Option<i32>,
) {
    sqlx::query(
        "SELECT (graphile_worker.add_job($1, '{}'::json, null, now(), 1, $2, 0, null::text[], 'replace')).id",
    )
    .bind(task)
    .bind(key)
    .execute(pool)
    .await
    .expect("enqueue");
    sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts,
                last_error = $2,
                updated_at = now() - make_interval(hours => $3),
                locked_at = CASE WHEN $4::bool THEN now() - make_interval(hours => $5) ELSE NULL END,
                locked_by = CASE WHEN $4::bool THEN 'worker' ELSE NULL END
          WHERE key = $1",
    )
    .bind(key)
    .bind(error)
    .bind(idle_hours)
    .bind(lock_hours.is_some())
    .bind(lock_hours.unwrap_or(0))
    .execute(pool)
    .await
    .expect("mark dead");
}

#[derive(Debug, sqlx::FromRow)]
struct JobState {
    attempts: i16,
    priority: i16,
    last_error: Option<String>,
    locked: bool,
}

async fn job_state(pool: &PgPool, key: &str) -> JobState {
    sqlx::query_as(
        "SELECT attempts, priority, last_error, locked_at IS NOT NULL AS locked
           FROM graphile_worker._private_jobs WHERE key = $1",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .expect("job")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn queue_status_reports_every_task_and_a_dead_job(pool: PgPool) {
    install_graphile(&pool).await;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let empty = run(&["memory", "queue-status"], &environment).await;
    assert_eq!(empty.exit, Exit::Ok, "{}", empty.stderr);
    assert_eq!(
        empty.stdout,
        "extract-wiki pending=0 running=0 scheduled=0 dead=0 recent_dead=0\n\
         compact-conversation pending=0 running=0 scheduled=0 dead=0 recent_dead=0\n\
         embed-target pending=0 running=0 scheduled=0 dead=0 recent_dead=0\n\
         synthesize-tool-call pending=0 running=0 scheduled=0 dead=0 recent_dead=0\n\
         file-conversation pending=0 running=0 scheduled=0 dead=0 recent_dead=0\n"
    );
    dead_job(&pool, "extract-wiki", "dead-wiki", "wiki failed", 25, None).await;
    let json = run(&["memory", "queue-status", "--json"], &environment).await;
    let rows: Vec<memory_postgres::queue::QueueHealth> =
        serde_json::from_str(json.stdout.trim()).expect("health");
    assert_eq!(rows.len(), 5);
    let wiki = rows.first().expect("extract-wiki");
    assert_eq!(wiki.task.to_string(), "extract-wiki");
    assert_eq!(wiki.dead, 1);
    assert_eq!(wiki.recent_dead, 1);
    assert_eq!(wiki.last_error.as_deref(), Some("wiki failed"));
    let next = rows.get(1).expect("second task");
    assert_eq!(next.dead, 0);
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn retry_failed_matches_a_substring_and_clears_locks(pool: PgPool) {
    install_graphile(&pool).await;
    dead_job(&pool, "extract-wiki", "wiki-open", "wiki failed", 30, None).await;
    dead_job(
        &pool,
        "extract-wiki",
        "wiki-locked",
        "wiki failed",
        25,
        Some(0),
    )
    .await;
    dead_job(&pool, "extract-wiki", "other", "other boom", 25, None).await;
    dead_job(
        &pool,
        "compact-conversation",
        "other-task",
        "wiki failed",
        25,
        None,
    )
    .await;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let missed = run(
        &[
            "memory",
            "retry-failed",
            "--task=extract-wiki",
            "--error",
            "Wiki",
            "--dry-run",
            "--json",
        ],
        &environment,
    )
    .await;
    assert_eq!(missed.stdout, "{\"reset\":0,\"dry_run\":true}\n");
    let limited = run(
        &[
            "memory",
            "retry-failed",
            "--task",
            "extract-wiki",
            "--error",
            "wiki",
            "--limit",
            "1",
        ],
        &environment,
    )
    .await;
    assert_eq!(limited.stdout, "reset 1 dead job(s)\n");
    assert_eq!(job_state(&pool, "wiki-open").await.attempts, 0);
    assert!(job_state(&pool, "wiki-locked").await.locked);
    let rest = run(
        &[
            "memory",
            "retry-failed",
            "--task",
            "extract-wiki",
            "--error",
            "wiki",
            "--json",
        ],
        &environment,
    )
    .await;
    assert_eq!(rest.stdout, "{\"reset\":1,\"dry_run\":false}\n");
    let cleared = job_state(&pool, "wiki-locked").await;
    assert_eq!(cleared.attempts, 0);
    assert_eq!(cleared.last_error, None);
    assert!(!cleared.locked);
    assert_eq!(job_state(&pool, "other").await.attempts, 1);
    assert_eq!(job_state(&pool, "other-task").await.attempts, 1);
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn requeue_revives_idle_rows_and_reports_the_cap(pool: PgPool) {
    install_graphile(&pool).await;
    dead_job(&pool, "extract-wiki", "old-open", "stale", 25, None).await;
    dead_job(&pool, "extract-wiki", "stale-lock", "stale", 25, Some(5)).await;
    dead_job(&pool, "extract-wiki", "fresh-lock", "stale", 25, Some(0)).await;
    dead_job(&pool, "extract-wiki", "recent", "stale", 1, None).await;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let revived = run(
        &["memory", "requeue", "--task", "extract-wiki"],
        &environment,
    )
    .await;
    assert_eq!(revived.stdout, "requeued 2 dead job(s)\n");
    let open = job_state(&pool, "old-open").await;
    assert_eq!((open.attempts, open.priority), (0, 10));
    assert_eq!(open.last_error.as_deref(), Some("stale"));
    let unlocked = job_state(&pool, "stale-lock").await;
    assert_eq!(
        (unlocked.attempts, unlocked.priority, unlocked.locked),
        (0, 10, false)
    );
    assert_eq!(job_state(&pool, "fresh-lock").await.attempts, 1);
    assert_eq!(job_state(&pool, "recent").await.attempts, 1);
    let clamped = run(
        &[
            "memory",
            "requeue",
            "--task",
            "file-conversation",
            "--limit",
            "5000",
            "--dry-run",
            "--json",
        ],
        &environment,
    )
    .await;
    assert_eq!(
        clamped.stdout,
        "{\"requeued\":0,\"dry_run\":true,\"limit\":1000}\n"
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn tool_synth_enqueues_each_empty_assistant_once(pool: PgPool) {
    install_graphile(&pool).await;
    let message = seed_tool_message(&pool).await;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let first = run(&["memory", "backfill-tool-synth"], &environment).await;
    assert_eq!(first.stdout, "enqueued 1 synthesize-tool-call job(s)\n");
    let key = format!("tool-synth-{message}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs WHERE key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(count, 1);
    let second = run(&["memory", "backfill-tool-synth", "--json"], &environment).await;
    assert_eq!(second.stdout, "{\"enqueued\":0,\"dry_run\":false}\n");
}

async fn seed_tool_message(pool: &PgPool) -> Uuid {
    let mut transaction = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *transaction)
        .await
        .expect("defer");
    let conversation = Uuid::new_v4();
    let message = Uuid::new_v4();
    sqlx::query("INSERT INTO rh_conversations (id, session_key, agent, owner_user_id) VALUES ($1, $2, 'main', 'owner')")
        .bind(conversation)
        .bind(format!("tool:{conversation}"))
        .execute(&mut *transaction)
        .await
        .expect("conversation");
    sqlx::query(
        "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, tool_name, owner_user_id)
         VALUES ($1, $2, 'main', 'cli', 'assistant', '', 'shell', 'owner')",
    )
    .bind(message)
    .bind(conversation)
    .execute(&mut *transaction)
    .await
    .expect("empty");
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, tool_name, owner_user_id)
         VALUES ($1, 'main', 'cli', 'assistant', 'hello', 'shell', 'owner')",
    )
    .bind(conversation)
    .execute(&mut *transaction)
    .await
    .expect("filled");
    transaction.commit().await.expect("commit");
    message
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn export_round_trip_refuses_a_full_target_and_userinfo_is_local(pool: PgPool) {
    let id = seed_round_trip(&pool).await;
    let directory = tempfile::tempdir().expect("directory");
    let out = directory.path().join("memory.dump.gz");
    let out_text = out.display().to_string();
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let future = directory.path().join("future.dump.gz");
    assert_eq!(
        run(
            &[
                "memory",
                "export",
                "--since",
                "2099-01-01T00:00:00Z",
                "--out",
                &future.display().to_string()
            ],
            &environment,
        )
        .await
        .exit,
        Exit::Ok
    );
    assert!(!gunzip(&std::fs::read(&future).expect("gzip")).contains(SESSION));
    let wrote = run(&["memory", "export", "--out", &out_text], &environment).await;
    assert_eq!(wrote.stdout, format!("wrote {out_text}\n"));
    let plain = gunzip(&std::fs::read(&out).expect("gzip"));
    let host = nix::unistd::gethostname().expect("host");
    let host = host.to_str().expect("utf-8");
    assert!(plain.contains("-- RivetHub memory export\n-- source local\n"));
    assert!(plain.contains(&format!("-- host {host}\n")));
    assert!(plain.contains("COPY public.ros_conversations"));
    assert!(plain.contains(SESSION));
    assert!(plain.contains(BODY));
    assert!(plain.ends_with("-- PostgreSQL database dump complete\n"));
    sqlx::query("DELETE FROM rh_messages")
        .execute(&pool)
        .await
        .expect("delete messages");
    sqlx::query("DELETE FROM rh_conversations")
        .execute(&pool)
        .await
        .expect("delete conversations");
    let preview = run(&["memory", "import", &out_text, "--dry-run"], &environment).await;
    assert_eq!(preview.exit, Exit::Ok, "{}", preview.stderr);
    assert_eq!(count_conversations(&pool).await, 0);
    let imported = run(&["memory", "import", &out_text], &environment).await;
    assert_eq!(imported.exit, Exit::Ok, "{}", imported.stderr);
    assert_eq!(count_conversations(&pool).await, 1);
    let body: String =
        sqlx::query_scalar("SELECT content FROM rh_messages WHERE conversation_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("body");
    assert_eq!(body, BODY);
    let occupied = run(&["memory", "import", &out_text], &environment).await;
    assert!(occupied.stderr.contains("not empty"));
}

async fn seed_round_trip(pool: &PgPool) -> Uuid {
    let mut transaction = pool.begin().await.expect("begin");
    sqlx::query("SET LOCAL rivet.defer_embed_enqueue = 'on'")
        .execute(&mut *transaction)
        .await
        .expect("defer");
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO rh_conversations (id, session_key, agent, owner_user_id) VALUES ($1, $2, 'main', 'owner')")
        .bind(id)
        .bind(SESSION)
        .execute(&mut *transaction)
        .await
        .expect("conversation");
    sqlx::query(
        "INSERT INTO rh_messages (conversation_id, agent, channel, role, content, owner_user_id) VALUES ($1, 'main', 'cli', 'user', $2, 'owner')",
    )
    .bind(id)
    .bind(BODY)
    .execute(&mut *transaction)
    .await
    .expect("message");
    transaction.commit().await.expect("commit");
    id
}

async fn count_conversations(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM rh_conversations")
        .fetch_one(pool)
        .await
        .expect("count")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn queue_commands_fail_when_graphile_is_absent(pool: PgPool) {
    sqlx::query("DROP SCHEMA graphile_worker CASCADE")
        .execute(&pool)
        .await
        .expect("drop the queue schema");
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let report = run(&["memory", "queue-status"], &environment).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report
            .stderr
            .contains("the graphile_worker schema is absent on this database")
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn env_url_beats_config_and_the_legacy_name_warns_once(pool: PgPool) {
    install_graphile(&pool).await;
    let url = url_of(&pool);
    let (home, environment) = env(&[("RIVETHUB_PG_URL", &url)]);
    support::config(
        &home,
        "memory:\n  postgres:\n    connection_string: postgres://127.0.0.1:1/closed\n",
    );
    let preferred = run(&["memory", "queue-status"], &environment).await;
    assert_eq!(preferred.exit, Exit::Ok, "{}", preferred.stderr);
    assert_eq!(preferred.stderr, "");
    let (legacy_home, legacy) = env(&[("RIVETOS_PG_URL", &url)]);
    let warned = run(&["memory", "queue-status"], &legacy).await;
    assert_eq!(warned.exit, Exit::Ok, "{}", warned.stderr);
    assert!(warned.stderr.starts_with(
        "warning: RIVETOS_PG_URL is deprecated — rename it to RIVETHUB_PG_URL; the legacy name is read only while RIVETHUB_PG_URL is unset.\n"
    ));
    let both = legacy.with_override("RIVETHUB_PG_URL", &url);
    let quiet = run(&["memory", "queue-status"], &both).await;
    assert_eq!(quiet.stderr, "");
    drop(legacy_home);
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn config_url_is_used_when_the_environment_is_unset(pool: PgPool) {
    install_graphile(&pool).await;
    let (home, environment) = env(&[]);
    support::config(
        &home,
        &format!(
            "memory:\n  postgres:\n    connection_string: \"{}\"\n",
            url_of(&pool)
        ),
    );
    let report = run(&["memory", "queue-status"], &environment).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
}

#[tokio::test]
async fn memory_settle_reports_the_persisted_revision() -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::SummaryStore as _;
    use testkit::settlement::{batch, message};

    let (home, environment) = env(&[]);
    let path = home.path().join("settle.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?;
    let owner = kernel::owner::Owner::new(wire::gateway::users::DEFAULT_OWNER_ID.to_owned())?;
    let key = "cli:invented-session";
    let captured = store
        .capture(
            batch(key, vec![message("entry", "2026-01-01T00:00:00Z")?]),
            &owner,
            None,
            jiff::Timestamp::now(),
        )
        .await?;
    let id = captured.conversation_id.parse()?;
    let db = path.to_str().ok_or("sqlite path")?;
    let first = memory(
        &["settle", key, "--sqlite", db, "--json"],
        &environment,
        false,
    )
    .await;
    assert_eq!(first.exit, Exit::Ok, "{}", first.stderr);
    let result: kernel::memory::settlement::SettleResult = serde_json::from_str(&first.stdout)?;
    assert_eq!(
        result.status,
        kernel::memory::settlement::SettleStatus::Settled
    );
    assert_eq!(result.input_revision, 1);
    let second = memory(&["settle", key, "--sqlite", db], &environment, false).await;
    assert_eq!(second.exit, Exit::Ok);
    assert!(second.stdout.contains("revision 1 already settled"));
    assert!(store.summary_eligible(id, 1).await?);
    let missing = memory(
        &["settle", "cli:unknown", "--sqlite", db],
        &environment,
        false,
    )
    .await;
    assert_eq!(missing.exit, Exit::Failure);
    Ok(())
}

#[tokio::test]
async fn resummarize_arms_one_historical_opening_once() {
    #[derive(serde::Deserialize)]
    struct ArmReport {
        armed: bool,
    }

    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at)
         VALUES ('00000000-0000-0000-0000-000000000001','cli:historical','fixture','cli','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z'),
                ('00000000-0000-0000-0000-000000000002','cli:trashed','fixture','cli','2026-10-10T00:00:00Z','2026-10-10T00:00:00Z');
         INSERT INTO rh_summary_openings (conversation_id, content, historical_pending)
         SELECT id, 'Invented equipment review', 1 FROM ros_conversations;
         INSERT INTO rh_session_trash (session_key, trashed_at) VALUES ('cli:trashed','2026-01-01Z');",
    ).unwrap();
    let (_home, environment) = env(&[]);
    let arm = async |id, json| {
        let mut args = vec![
            "memory",
            "resummarize",
            "arm-opening",
            id,
            "--sqlite",
            database.to_str().unwrap(),
        ];
        if json {
            args.push("--json");
        }
        run(&args, &environment).await
    };
    let armed = arm("00000000-0000-0000-0000-000000000001", false).await;
    assert_eq!(armed.exit, Exit::Ok, "{}", armed.stderr);
    assert!(armed.stdout.contains("Opening armed"), "{}", armed.stdout);
    let refused = arm("00000000-0000-0000-0000-000000000002", false).await;
    assert_eq!(refused.exit, Exit::Failure);
    assert!(
        refused
            .stderr
            .contains("cannot be filed because it is in the trash")
    );
    assert_eq!(
        scalar(
            &database,
            "SELECT count(*) FROM rh_summary_openings WHERE filing_armed AND title_armed"
        ),
        1
    );
    assert_eq!(
        scalar(
            &database,
            "SELECT count(*) FROM rh_summary_openings WHERE historical_pending"
        ),
        1
    );
    conn.execute_batch("UPDATE rh_summary_openings SET filing_armed = 0, title_armed = 0")
        .unwrap();
    let retry = arm("00000000-0000-0000-0000-000000000001", true).await;
    assert_eq!(retry.exit, Exit::Ok, "{}", retry.stderr);
    let result: ArmReport = serde_json::from_str(&retry.stdout).unwrap();
    assert!(result.armed);
    assert_eq!(
        scalar(
            &database,
            "SELECT count(*) FROM rh_summary_openings WHERE filing_armed OR title_armed"
        ),
        0
    );
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn cross_store_summary_import_reports_and_repairs_vector_loss(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::{MemoryExport, SummaryStore};
    use memory_postgres::PostgresMemory;
    use std::sync::Arc;

    let key = [0xab; 32];
    let pg = Arc::new(PostgresMemory::new(pool.clone()).with_summary_index_key(key));
    let original = std::fs::read_to_string(testkit::recording("summary-archive/top-down.txt"))?;
    let cancel = CancellationToken::new();
    pg.import_memory_dump(&original, false, &cancel).await?;
    let part = uuid::uuid!("00000000-0000-4000-8000-000000000021");
    repair_imported_summary(pg.as_ref(), part).await?;
    let exporter = cli::memory::MemoryExporter::postgres(pg, url_of(&pool));
    let dump = tempfile::NamedTempFile::new()?;
    std::fs::write(
        dump.path(),
        gunzip(&exporter.export(None, cancel.clone()).await?),
    )?;
    let directory = tempfile::tempdir()?;
    let sqlite =
        memory_sqlite::SqliteStore::open(migrated(directory.path()))?.with_summary_index_key(key);
    let imported = sqlite
        .import_pg_dump(dump.path().to_path_buf(), cancel.clone())
        .await?;
    let ids = sqlite.repair_candidates(16).await?;
    assert_eq!(imported.summary_records_needing_reembedding, 2);
    assert_eq!(imported.summary_records_losing_passage_vectors, 1);
    assert_eq!(ids.len(), 2);
    for id in ids {
        repair_imported_summary(&sqlite, id).await?;
    }
    assert_eq!(sqlite.repair_candidates(16).await?, Vec::<Uuid>::new());
    let origin = kernel::memory::DumpOrigin {
        source: kernel::memory::DumpSource::Local,
        host: "fixture".to_owned(),
    };
    let pooled = sqlite.export_pg_dump(None, origin, cancel.clone()).await?;
    let second = import_fixture_database(&pool).await?;
    let pg = PostgresMemory::new(second).with_summary_index_key(key);
    let imported = pg
        .import_memory_dump(std::str::from_utf8(&pooled)?, false, &cancel)
        .await?;
    assert_eq!(imported.summary_records_needing_reembedding, 2);
    let ids = pg.repair_candidates(16).await?;
    assert_eq!(ids.len(), 2);
    for id in ids {
        repair_imported_summary(&pg, id).await?;
    }
    assert_eq!(pg.repair_candidates(16).await?, Vec::<Uuid>::new());
    Ok(())
}

const RELEASE_CONVERSATION: Uuid = uuid::uuid!("00000000-0000-4000-8000-000000000001");

async fn seed_release_fixture(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let original = std::fs::read_to_string(testkit::recording("summary-archive/top-down.txt"))?;
    memory_postgres::PostgresMemory::new(pool.clone())
        .import_memory_dump(&original, false, &CancellationToken::new())
        .await?;
    sqlx::query(
        r#"INSERT INTO rh_messages
           (conversation_id, agent, channel, role, tool_result, tool_args, created_at, owner_user_id)
         VALUES ($1, 'fixture', 'cli', 'tool', 'Pressure is steady.', NULL, '2026-10-10T12:05:00Z', 'alice'),
                ($1, 'fixture', 'cli', 'tool', NULL, '{"unit":"bar"}', '2026-10-10T12:06:00Z', 'alice')"#,
    )
    .bind(RELEASE_CONVERSATION)
    .execute(pool)
    .await?;
    sqlx::raw_sql(
        "UPDATE rh_summary_deliveries SET anchor_id = NULL, historical_backlog = true
         WHERE anchor_id IS NOT NULL;
         INSERT INTO rh_summary_deliveries (conversation_id, consumer, message_id, historical_backlog)
         SELECT m.conversation_id, consumers.name, m.id, true
         FROM rh_messages m CROSS JOIN (VALUES ('entity'), ('wiki')) AS consumers(name)
         WHERE m.role = 'tool';",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn release_fixture(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::summary_conversion::{ConversionControl, SummaryConversion};

    memory_postgres::PostgresMemory::new(pool.clone())
        .control(ConversionControl::Release {
            conversation: RELEASE_CONVERSATION,
        })
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn released_summary_deliveries_round_trip_into_both_stores(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::MemoryExport;
    use memory_postgres::PostgresMemory;
    use memory_sqlite::import::ImportTable;
    use std::sync::Arc;

    seed_release_fixture(&pool).await?;
    release_fixture(&pool).await?;
    let cancel = CancellationToken::new();
    let pg = Arc::new(PostgresMemory::new(pool.clone()));
    let exporter = cli::memory::MemoryExporter::postgres(pg, url_of(&pool));
    let dump_text = gunzip(&exporter.export(None, cancel.clone()).await?);
    let dump = tempfile::NamedTempFile::new()?;
    std::fs::write(dump.path(), &dump_text)?;

    let target = PostgresMemory::new(import_fixture_database(&pool).await?);
    let directory = tempfile::tempdir()?;
    let sqlite = memory_sqlite::SqliteStore::open(migrated(directory.path()))?;
    let (pg, sqlite) = tokio::join!(
        target.import_memory_dump(&dump_text, false, &cancel),
        sqlite.import_pg_dump(dump.path().to_path_buf(), cancel.clone()),
    );
    assert!(
        pg.is_ok() && sqlite.is_ok(),
        "PostgreSQL {pg:?}, SQLite {sqlite:?}"
    );
    assert_eq!(pg?.rows.get("summary_deliveries"), Some(&6));
    assert_eq!(sqlite?.rows.get(&ImportTable::SummaryDeliveries), Some(&6));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn merge_store_accepts_released_destination_with_uncovered_deliveries(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::owner::Owner;
    use memory_postgres::{PostgresMemory, merge::MergeRequest};

    seed_release_fixture(&pool).await?;
    release_fixture(&pool).await?;
    let source = import_fixture_database(&pool).await?;
    sqlx::query(
        "INSERT INTO rh_conversations (session_key, agent, channel, owner_user_id)
         VALUES ('cli:spare-tray', 'fixture', 'cli', 'alice')",
    )
    .execute(&source)
    .await?;
    let destination = PostgresMemory::new(pool.clone());
    let report = destination
        .merge_store(
            &PostgresMemory::new(source),
            &MergeRequest {
                owner: Owner::new("alice".to_owned())?,
                moves: std::collections::BTreeMap::new(),
                dry_run: false,
            },
            &CancellationToken::new(),
        )
        .await?;
    assert!(report.committed);
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_summary_deliveries WHERE historical_backlog")
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining, 4);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn resummarize_status_distinguishes_uncovered_deliveries_from_waiting_for_release(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DeliveryBacklog {
        delivery_rows_waiting_for_release: i64,
        delivery_rows_not_covered: i64,
    }

    seed_release_fixture(&pool).await?;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    for waiting in [1, 0] {
        let text = run(&["memory", "resummarize", "status"], &environment).await;
        assert_eq!(text.exit, Exit::Ok, "{}", text.stderr);
        assert!(
            text.stdout.contains(&format!(
                "{waiting} delivery rows waiting for release, 4 not covered by summaries"
            )),
            "{}",
            text.stdout
        );
        let json = run(&["memory", "resummarize", "status", "--json"], &environment).await;
        assert_eq!(json.exit, Exit::Ok, "{}", json.stderr);
        let progress: DeliveryBacklog = serde_json::from_str(&json.stdout)?;
        assert_eq!(progress.delivery_rows_waiting_for_release, waiting);
        assert_eq!(progress.delivery_rows_not_covered, 4);
        release_fixture(&pool).await?;
    }
    Ok(())
}

async fn repair_imported_summary(
    store: &dyn kernel::memory::SummaryStore,
    id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::summary_backend::{SummaryIndex, SummaryPassage};
    let input = store
        .repair_input(id)
        .await?
        .ok_or("missing repair input")?;
    let start = input.sources.first().ok_or("missing sources")?.1;
    let end = input.sources.last().ok_or("missing sources")?.1;
    let vector = vec![0.5; 1024];
    let index = SummaryIndex {
        passages: vec![SummaryPassage {
            content: input.content.clone(),
            chars: 0..i32::try_from(input.content.chars().count())?,
            earliest_at: start.min(end),
            latest_at: start.max(end),
            vector: vector.clone(),
        }],
        pooled: vector,
    };
    store.repair_index(input, index).await?;
    Ok(())
}

async fn import_fixture_database(pool: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    let name: String =
        sqlx::query_scalar("SELECT 'rh_import_' || left(md5(current_database()), 16)")
            .fetch_one(pool)
            .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {name}"
    )))
    .execute(pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(pool)
        .await?;
    let options = pool.connect_options().as_ref().clone().database(&name);
    let target = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await?;
    migrate::postgres_pool(&target).await?;
    Ok(target)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn starting_and_resuming_conversion_reset_an_idle_bucket_on_both_stores(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::summary_conversion::{ConversionControl, SummaryConversion};

    let pg = memory_postgres::PostgresMemory::new(pool.clone());
    let directory = tempfile::tempdir()?;
    let path = migrated(directory.path());
    let sqlite = memory_sqlite::SqliteStore::open(&path)?;
    let conn = rusqlite::Connection::open(&path)?;
    for action in [
        ConversionControl::Start {
            rate: Some(200_000),
            include_giants: false,
        },
        ConversionControl::Resume {
            include_giants: false,
        },
    ] {
        sqlx::query("UPDATE rh_summary_conversion SET bucket_at = '1970-01-01 00:00:00+00'")
            .execute(&pool)
            .await?;
        conn.execute_batch("UPDATE rh_summary_conversion SET bucket_at = '1970-01-01T00:00:00Z'")?;
        pg.control(action).await?;
        sqlite.control(action).await?;
        let pg_reset: bool = sqlx::query_scalar(
            "SELECT bucket_at >= clock_timestamp() - interval '30 seconds' FROM rh_summary_conversion",
        ).fetch_one(&pool).await?;
        assert!(pg_reset);
        assert_eq!(
            scalar(
                &path,
                "SELECT julianday(bucket_at) >= julianday('now', '-30 seconds') FROM rh_summary_conversion"
            ),
            1
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn conversion_status_counts_debits_without_provider_usage_on_both_stores(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use kernel::memory::summary_conversion::{
        ConversionCharge, ConversionControl, ConversionUsage, SummaryConversion,
    };

    sqlx::raw_sql(
        "INSERT INTO rh_conversations (session_key, agent, channel, owner_user_id)
         VALUES ('cli:ledger', 'fixture', 'cli', 'alice');
         INSERT INTO rh_messages (conversation_id, agent, channel, role, content, created_at, owner_user_id)
         SELECT id, 'fixture', 'cli', 'user', 'Measure the spring.', '2024-01-01', 'alice'
         FROM rh_conversations;",
    )
    .execute(&pool)
    .await?;
    let pg = memory_postgres::PostgresMemory::new(pool);
    let directory = tempfile::tempdir()?;
    let path = migrated(directory.path());
    let sqlite = memory_sqlite::SqliteStore::open(&path)?;
    rusqlite::Connection::open(path)?.execute_batch(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at)
         VALUES ('00000000-0000-4000-8000-000000000001', 'cli:ledger', 'fixture', 'cli',
                 '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z');
         INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
         VALUES ('00000000-0000-4000-8000-000000000002', '00000000-0000-4000-8000-000000000001',
                 'fixture', 'cli', 'user', 'Measure the spring.', '2024-01-01T00:00:00Z');",
    )?;

    let mut totals = Vec::new();
    for store in [&pg as &dyn SummaryConversion, &sqlite] {
        store
            .control(ConversionControl::Start {
                rate: Some(1000),
                include_giants: false,
            })
            .await?;
        let inactivity = jiff::SignedDuration::from_hours(24);
        assert!(store.claim(inactivity).await?.is_none(), "inventory pass");
        let lease = store.claim(inactivity).await?.ok_or("fixture lease")?;
        for estimate in [100, 50] {
            assert!(
                store
                    .admit(
                        lease,
                        Some(ConversionCharge {
                            estimate,
                            capacity: 1000
                        })
                    )
                    .await?
            );
        }
        store
            .report(ConversionUsage {
                estimate: 50,
                input: 40,
                output: 5,
            })
            .await?;
        let progress = store.status().await?;
        totals.push((
            progress.charged_tokens,
            progress.provider_input_tokens,
            progress.provider_output_tokens,
        ));
    }
    assert_eq!(totals, [(140, 40, 5); 2]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pause_and_resume_a_builder_from_the_command_line(pool: PgPool) {
    install_graphile(&pool).await;
    let (_home, environment) = env(&[("RIVETHUB_PG_URL", &url_of(&pool))]);
    let paused = run(&["memory", "pause", "entity-builder"], &environment).await;
    assert_eq!(paused.exit, Exit::Ok, "{}", paused.stderr);
    let lines: Vec<&str> = paused.stdout.lines().collect();
    let [categorizer, wiki, entity] = lines.as_slice() else {
        panic!("one line per pausable role: {}", paused.stdout);
    };
    assert_eq!(*categorizer, "categorizer running, 0 queued");
    assert_eq!(*wiki, "wiki-builder running, 0 queued");
    assert!(
        entity.starts_with("entity-builder paused since ") && entity.ends_with(" by cli, 0 queued"),
        "{entity}"
    );
    let again = run(&["memory", "pause", "entity-builder"], &environment).await;
    assert_eq!(again.stdout, paused.stdout);
    let status = run(&["memory", "pause", "--json"], &environment).await;
    let listed: wire::gateway::worker_pauses::WorkerPauses =
        serde_json::from_str(&status.stdout).expect("status json");
    assert_eq!(
        listed.roles.iter().filter(|role| role.paused).count(),
        1,
        "{}",
        status.stdout
    );

    let resumed = run(&["memory", "resume", "entity-builder"], &environment).await;
    assert_eq!(resumed.exit, Exit::Ok, "{}", resumed.stderr);
    assert!(
        resumed
            .stdout
            .ends_with("entity-builder running, 0 queued\n")
    );

    failure(
        &run(&["memory", "pause", "summarizer"], &environment).await,
        "summarizer cannot be paused; the pausable roles are categorizer, wiki-builder, entity-builder",
    );
    failure(
        &run(&["memory", "resume"], &environment).await,
        "usage: rh memory pause [<role>] [--json] | rh memory resume <role> [--json]; roles: categorizer, wiki-builder, entity-builder",
    );
}
