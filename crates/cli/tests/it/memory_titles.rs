#![cfg(test)]

use crate::support;

use std::path::Path;
use std::path::PathBuf;

use cli::Exit;
use rusqlite::OptionalExtension as _;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TitlesBody {
    titled: u64,
    dry_run: bool,
}

#[tokio::test]
async fn backfill_titles_names_summarized_sqlite_conversations() {
    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    seed(
        &database,
        "untitled",
        None,
        "## Rotated the signing keys. Then checked the logs.",
    );
    seed(
        &database,
        "named",
        Some("Named by the hook"),
        "Something else entirely.",
    );
    let home = support::home();
    let environment = support::environment(&home);
    let path = database.to_string_lossy().to_string();
    let preview = support::run(
        &["memory", "backfill-titles", "--sqlite", &path, "--dry-run"],
        &environment,
    )
    .await;
    assert_eq!(preview.exit, Exit::Ok, "{}", preview.stderr);
    assert_eq!(preview.stdout, "would title 1 conversation(s)\n");
    assert_eq!(title(&database, "untitled"), None);
    let applied = support::run(
        &["memory", "backfill-titles", "--sqlite", &path],
        &environment,
    )
    .await;
    assert_eq!(applied.exit, Exit::Ok, "{}", applied.stderr);
    assert_eq!(applied.stdout, "titled 1 conversation(s)\n");
    assert_eq!(
        title(&database, "untitled").as_deref(),
        Some("Rotated the signing keys.")
    );
    assert_eq!(
        title(&database, "named").as_deref(),
        Some("Named by the hook")
    );
    let again = support::run(
        &["memory", "backfill-titles", "--sqlite", &path, "--json"],
        &environment,
    )
    .await;
    assert_eq!(again.exit, Exit::Ok, "{}", again.stderr);
    let body: TitlesBody = serde_json::from_str(&again.stdout).expect("json");
    assert_eq!(body.titled, 0);
    assert!(!body.dry_run);
}

#[tokio::test]
async fn backfill_titles_rejects_unknown_options() {
    let home = support::home();
    let environment = support::environment(&home);
    let report = support::run(&["memory", "backfill-titles", "--frob"], &environment).await;
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(report.stderr, "Error: Unknown option: --frob\n");
}

#[tokio::test]
async fn model_titles_need_a_model_url_and_postgres() {
    let directory = tempfile::tempdir().expect("directory");
    let database = migrated(directory.path());
    let path = database.to_string_lossy().to_string();
    let home = support::home();
    let environment = support::environment(&home);
    let half = support::run(
        &["memory", "backfill-titles", "--model", "fake-chat"],
        &environment,
    )
    .await;
    assert_eq!(half.exit, Exit::Failure);
    assert_eq!(
        half.stderr,
        "Error: rh memory backfill-titles names titles with both --model <name> and --model-url <url>\n"
    );
    let sqlite = support::run(
        &[
            "memory",
            "backfill-titles",
            "--sqlite",
            &path,
            "--model",
            "fake-chat",
            "--model-url",
            "http://127.0.0.1:9/v1",
        ],
        &environment,
    )
    .await;
    assert_eq!(sqlite.exit, Exit::Failure);
    assert_eq!(
        sqlite.stderr,
        "Error: This command needs a datahub (Postgres).\n"
    );
}

fn migrated(directory: &Path) -> PathBuf {
    let path = directory.join("memory.sqlite");
    migrate::sqlite(&path).expect("migrate");
    path
}

fn seed(path: &Path, id: &str, title: Option<&str>, leaf: &str) {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    connection
        .execute(
            "INSERT INTO ros_conversations (id, session_key, agent, title, settings, created_at, updated_at) VALUES (?1, ?1, 'main', ?2, '{}', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z')",
            (id, title),
        )
        .expect("conversation");
    connection
        .execute(
            "INSERT INTO ros_summaries (id, conversation_id, depth, content, kind, message_count, earliest_at, latest_at, model, created_at, pipeline_version) VALUES (?1, ?2, 0, ?3, 'leaf', 0, '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z', 'fake-chat', '2020-01-01T00:00:00.000Z', 5)",
            (format!("leaf-{id}"), id, leaf),
        )
        .expect("leaf");
}

fn title(path: &Path, id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open(path).expect("sqlite");
    connection
        .query_row(
            "SELECT title FROM ros_conversations WHERE id = ?1",
            [id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .expect("title")
        .expect("conversation")
}
