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
        assert!(!report.stdout.contains("doctor"));
    }
}

#[tokio::test]
async fn memory_rejects_unknown_options_and_incomplete_flags() {
    let (_home, environment) = env(&[]);
    let missing = run(&["memory", "queue-status"], &environment).await;
    failure(&missing, "RIVETHUB_PG_URL is required.");
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
    failure(&report, "RIVETHUB_PG_URL is required.");
}

#[tokio::test]
async fn cloud_import_exits_2_and_sqlite_wins() {
    let (_home, environment) =
        env(&[("RIVETHUB_PG_URL", "postgres://user:secret@rivetos.cloud/db")]);
    let refused = memory(&["import", "/no/such/memory.dump"], &environment, false).await;
    assert_eq!(refused.exit, Exit::Memory);
    assert_eq!(refused.stdout, "");
    assert_eq!(
        refused.stderr,
        "RIVETHUB_PG_URL host is rivetos.cloud; tenant roles cannot import summaries/wiki. Use `rh cloud import <file>` instead.\n"
    );
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
async fn sqlite_import_is_idempotent_only_when_empty_and_dry_run_rolls_back() {
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
    let again = run(
        &["memory", "import", &recording(), "--sqlite", &path],
        &environment,
    )
    .await;
    assert_eq!(again.exit, Exit::Failure);
    assert!(again.stderr.contains("not empty"));
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
    let mut disguised: url::Url = pool.connect_options().as_ref().to_url_lossy();
    disguised.set_username("rivetos.cloud").expect("user");
    let (_home, disguised_env) = env(&[("RIVETHUB_PG_URL", disguised.as_ref())]);
    let local = run(&["memory", "import", &out_text], &disguised_env).await;
    assert_eq!(local.exit, Exit::Failure, "{}", local.stderr);
    assert!(
        !local.stderr.contains("tenant roles cannot import"),
        "{}",
        local.stderr
    );
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
