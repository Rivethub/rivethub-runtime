#![cfg(test)]

use crate::support;

use cli::Exit;
use tokio_util::sync::CancellationToken;

const DANA: &str = "00000000-0000-4000-8000-00000000000a";
const OWNER: &str = "00000000-0000-4000-8000-00000000000b";
const STAMP: &str = "2026-01-01T00:00:00.000Z";

#[tokio::test]
async fn settle_without_a_config_file_settles_only_the_users_file_owner() {
    let home = support::home();
    let path = two_owners(&home);
    let users = home.path().join("users.json");
    std::fs::write(
        &users,
        r#"{"ownerUserId":"dana","unmappedIsOwner":true,"users":{}}"#,
    )
    .expect("users");
    let environment =
        missing_config(&home).with_override("RIVETHUB_USERS_FILE", users.display().to_string());
    let theirs = memory(&["settle", OWNER, "--sqlite", &path], &environment).await;
    assert_eq!(theirs.exit, Exit::Failure, "{}", theirs.stdout);
    let own = memory(&["settle", DANA, "--sqlite", &path], &environment).await;
    assert_eq!((own.exit, own.stderr.as_str()), (Exit::Ok, ""));
}

#[tokio::test]
async fn a_directory_named_as_the_config_is_an_error_for_bundle() {
    let home = support::home();
    let path = two_owners(&home);
    let out = home.path().join("bundle.json");
    let environment = support::environment(&home)
        .with_override("RIVETHUB_CONFIG", home.path().display().to_string());
    let report = memory(
        &[
            "bundle",
            "--sqlite",
            &path,
            "--out",
            &out.display().to_string(),
        ],
        &environment,
    )
    .await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(!out.try_exists().expect("stat"));
}

#[tokio::test]
async fn settle_with_an_unreadable_config_does_not_open_the_store() {
    let home = support::home();
    let path = two_owners(&home);
    let conn = rusqlite::Connection::open(&path).expect("open");
    let mode: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .expect("journal");
    assert_eq!(mode, "delete");
    drop(conn);
    let config = home.path().join("broken.yaml");
    std::fs::write(&config, "memory: [\n").expect("config");
    let environment =
        support::environment(&home).with_override("RIVETHUB_CONFIG", config.display().to_string());
    let report = memory(&["settle", OWNER, "--sqlite", &path], &environment).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("reopen");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal");
    assert_eq!(mode, "delete");
}

#[tokio::test]
async fn pause_and_resume_say_the_store_is_busy_under_a_held_write_lock() {
    let home = support::home();
    let path = home.path().join("queue.sqlite");
    migrate::sqlite(&path).expect("schema");
    let writer = rusqlite::Connection::open(&path).expect("writer");
    writer.execute_batch("BEGIN IMMEDIATE").expect("lock");
    let environment = support::environment(&home);
    let path = path.display().to_string();
    let mut said = Vec::new();
    for verb in ["pause", "resume"] {
        let report = memory(&[verb, "wiki-builder", "--sqlite", &path], &environment).await;
        said.push(report.stderr);
    }
    writer.execute_batch("ROLLBACK").expect("unlock");
    let busy = "Error: the store is busy; run this command again\n".to_owned();
    assert_eq!(said, [busy.clone(), busy]);
}

fn two_owners(home: &tempfile::TempDir) -> String {
    let path = home.path().join("memory.sqlite");
    migrate::sqlite(&path).expect("schema");
    let conn = rusqlite::Connection::open(&path).expect("open");
    for (session, owner) in [(DANA, "dana"), (OWNER, "owner")] {
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, owner_user_id, created_at, updated_at)
             VALUES (?1, ?1, 'main', ?2, ?3, ?3)",
            rusqlite::params![session, owner, STAMP],
        )
        .expect("conversation");
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, metadata, created_at)
             VALUES (?1, ?2, 'main', 'cli', 'user', 'A short note', '{}', ?3)",
            rusqlite::params![format!("message-{owner}"), session, STAMP],
        )
        .expect("message");
        conn.execute(
            "INSERT INTO rh_summary_openings (conversation_id, content) VALUES (?1, 'An opening')",
            [session],
        )
        .expect("opening");
    }
    path.display().to_string()
}

fn missing_config(home: &tempfile::TempDir) -> config::Environment {
    support::environment(home).with_override(
        "RIVETHUB_CONFIG",
        home.path().join("missing.yaml").display().to_string(),
    )
}

async fn memory(parts: &[&str], environment: &config::Environment) -> cli::Report {
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(&args, environment, &CancellationToken::new(), false).await
}
