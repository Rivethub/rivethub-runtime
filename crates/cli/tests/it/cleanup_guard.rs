#![cfg(test)]

use std::os::unix::fs::MetadataExt as _;

use crate::support;

use cli::Exit;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn propose_refuses_the_live_database_under_another_url(pool: PgPool) {
    let live: url::Url = pool.connect_options().as_ref().to_url_lossy();
    let name = live.path().trim_start_matches('/').to_owned();
    let mut letters = name.bytes();
    let first = letters.next().expect("a database name");
    let rest = String::from_utf8(letters.collect()).expect("utf-8 name");
    let mut disguised = live.clone();
    disguised.set_path(&format!("/%{first:02X}{rest}"));
    let home = support::home();
    let environment = support::environment(&home).with_override("RIVETHUB_PG_URL", live.as_str());
    let refused = propose(&environment, disguised.as_str()).await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stdout);
    assert!(refused.stderr.contains(LIVE), "{}", refused.stderr);
    let mut elsewhere = live.clone();
    elsewhere.set_path("/postgres");
    let other = propose(&environment, elsewhere.as_str()).await;
    assert!(!other.stderr.contains(LIVE), "{}", other.stderr);
}

const LIVE: &str = "refusing to write cleanup proposals to the node's live memory database";

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn propose_treats_the_configured_url_as_live_when_the_environment_is_unset(pool: PgPool) {
    let live: url::Url = pool.connect_options().as_ref().to_url_lossy();
    let home = support::home();
    support::config(
        &home,
        &format!("memory:\n  backend: postgres\n  postgres:\n    connection_string: \"{live}\"\n"),
    );
    let refused = propose(&support::environment(&home), live.as_str()).await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stdout);
    assert!(refused.stderr.contains(LIVE), "{}", refused.stderr);
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn propose_treats_the_environment_url_as_live(pool: PgPool) {
    let live: url::Url = pool.connect_options().as_ref().to_url_lossy();
    let home = support::home();
    support::config(
        &home,
        "memory:\n  backend: postgres\n  postgres:\n    connection_string: \"postgres://127.0.0.1:1/closed\"\n",
    );
    let environment = support::environment(&home).with_override("RIVETHUB_PG_URL", live.as_str());
    let refused = propose(&environment, live.as_str()).await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stdout);
    assert!(refused.stderr.contains(LIVE), "{}", refused.stderr);
    let mut elsewhere = live.clone();
    elsewhere.set_path("/postgres");
    let other = propose(&environment, elsewhere.as_str()).await;
    assert!(!other.stderr.contains(LIVE), "{}", other.stderr);
}

#[tokio::test]
async fn propose_sqlite_refuses_when_no_live_path_is_configured() {
    let home = support::home();
    let database = home.path().join("old.sqlite");
    std::fs::write(&database, b"untouched").expect("sqlite placeholder");
    let out = tempfile::tempdir().expect("out folder");
    let database_text = database.to_str().expect("utf-8 path");
    let out_text = out.path().to_str().expect("utf-8 path");
    let parts = [
        "cleanup",
        "--propose",
        "--sqlite",
        database_text,
        "--out",
        out_text,
        "--model-url",
        "http://127.0.0.1:9/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    let refused = cli::memory::execute(
        &args,
        &support::environment(&home),
        &CancellationToken::new(),
        false,
    )
    .await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("no sqlite path is configured"),
        "{}",
        refused.stderr
    );
    assert_eq!(std::fs::read(&database).expect("file"), b"untouched");
}

#[tokio::test]
async fn propose_sqlite_refuses_a_hard_link_of_the_live_file() {
    let home = support::home();
    let live = home.path().join("memory.sqlite");
    std::fs::write(&live, b"live").expect("live file");
    let link = home.path().join("alias.sqlite");
    std::fs::hard_link(&live, &link).expect("hard link");
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            live.display()
        ),
    );
    let refused = propose_sqlite(&support::environment(&home), &link).await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stdout);
    assert!(refused.stderr.contains(LIVE), "{}", refused.stderr);
    assert_eq!(std::fs::read(&live).expect("file"), b"live");
}

#[tokio::test]
async fn propose_rejects_sqlite_and_database_together() {
    let home = support::home();
    let database = home.path().join("other.sqlite");
    std::fs::write(&database, b"other").expect("sqlite placeholder");
    let out = tempfile::tempdir().expect("out folder");
    let database_text = database.to_str().expect("utf-8 path");
    let out_text = out.path().to_str().expect("utf-8 path");
    let parts = [
        "cleanup",
        "--propose",
        "--database",
        "postgres://127.0.0.1/other",
        "--sqlite",
        database_text,
        "--out",
        out_text,
        "--model-url",
        "http://127.0.0.1:9/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    let refused = cli::memory::execute(
        &args,
        &support::environment(&home),
        &CancellationToken::new(),
        false,
    )
    .await;
    assert_eq!(refused.exit, Exit::Failure, "{}", refused.stdout);
    assert!(
        refused.stderr.contains("unexpected argument: --sqlite"),
        "{}",
        refused.stderr
    );
    assert_eq!(std::fs::read(&database).expect("file"), b"other");
}

async fn propose_sqlite(
    environment: &config::Environment,
    database: &std::path::Path,
) -> cli::Report {
    let out = tempfile::tempdir().expect("out folder");
    let database_text = database.to_str().expect("utf-8 path");
    let out_text = out.path().to_str().expect("utf-8 path");
    let parts = [
        "cleanup",
        "--propose",
        "--sqlite",
        database_text,
        "--out",
        out_text,
        "--model-url",
        "http://127.0.0.1:9/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(
        args.as_slice(),
        environment,
        &CancellationToken::new(),
        false,
    )
    .await
}

#[tokio::test]
async fn cleanup_refuses_checkpoints_when_the_sqlite_file_is_missing() {
    let home = support::home();
    let out = home.path().join("cleanup");
    seed_finished_checkpoints(&out);
    let missing = home.path().join("missing.sqlite");
    let report = dry_run(&home, &missing, &out).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(
        report.stderr.contains("not initialised"),
        "{}",
        report.stderr
    );
    assert!(!out.join("report.md").is_file());
    assert!(!out.join("target.json").is_file());
}

#[tokio::test]
async fn cleanup_refuses_an_unstamped_checkpoint_directory() {
    let home = support::home();
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database).expect("schema");
    let out = home.path().join("cleanup");
    seed_finished_checkpoints(&out);
    let report = dry_run(&home, &database, &out).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(report.stderr.contains(UNSTAMPED), "{}", report.stderr);
    assert!(!out.join("report.md").is_file());
}

#[tokio::test]
async fn cleanup_refuses_checkpoints_stamped_for_another_sqlite_file() {
    let home = support::home();
    let first = home.path().join("first.sqlite");
    let second = home.path().join("second.sqlite");
    migrate::sqlite(&first).expect("first");
    migrate::sqlite(&second).expect("second");
    let out = home.path().join("cleanup");
    seed_finished_checkpoints(&out);
    std::fs::write(out.join("target.json"), sqlite_stamp(&first)).expect("stamp");
    let report = dry_run(&home, &second, &out).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(report.stderr.contains(MISMATCH), "{}", report.stderr);
    assert!(!out.join("report.md").is_file());
}

#[tokio::test]
async fn a_proposal_receipt_without_an_identity_is_not_already_written() {
    let home = support::home();
    let live = home.path().join("live.sqlite");
    let other = home.path().join("other.sqlite");
    migrate::sqlite(&live).expect("live");
    migrate::sqlite(&other).expect("other");
    support::config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            live.display()
        ),
    );
    let out = home.path().join("cleanup");
    seed_finished_checkpoints(&out);
    std::fs::write(out.join("target.json"), sqlite_stamp(&other)).expect("stamp");
    std::fs::write(
        out.join("proposed.json"),
        r#"{"batch":"old","proposals":1,"database":"/tmp/other.sqlite"}"#,
    )
    .expect("receipt");
    let report = propose_at(&home, &other, &out).await;
    assert_eq!(report.exit, Exit::Failure, "{}", report.stdout);
    assert!(report.stderr.contains(UNSTAMPED), "{}", report.stderr);
    assert!(!report.stdout.contains("already written"));
}

const UNSTAMPED: &str =
    "this --out has no record of which database it was written for; start a new --out";
const MISMATCH: &str = "this --out was written for a different database; start a new --out";

fn seed_finished_checkpoints(out: &std::path::Path) {
    std::fs::create_dir_all(out).expect("out");
    let tree = r#"{"topics":[],"merges":[],"junk":[]}"#;
    std::fs::write(out.join("tree.json"), tree).expect("tree");
    std::fs::write(
        out.join("filings.json"),
        format!(r#"{{"tree":{tree},"lifts":[],"filings":[],"generic":[]}}"#),
    )
    .expect("filings");
    std::fs::write(
        out.join("folds.json"),
        r#"{"pages":[],"sections":[],"junk":[]}"#,
    )
    .expect("folds");
    std::fs::write(
        out.join("snapshot.json"),
        r#"{"takenAt":"2026-01-01T00:00:00Z","conversations":[],"topics":[],"redirects":[],"placements":[],"corrections":[],"projects":[],"refused":[],"pages":[],"sections":[]}"#,
    )
    .expect("snapshot");
}

fn sqlite_stamp(path: &std::path::Path) -> String {
    let meta = std::fs::metadata(path).expect("metadata");
    format!(
        r#"{{"kind":"sqlite","device":{},"inode":{}}}"#,
        meta.dev(),
        meta.ino()
    )
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_postgres_cleanup_stamps_its_checkpoint_directory(pool: PgPool) {
    let url = pool.connect_options().as_ref().to_url_lossy();
    let home = support::home();
    let out = home.path().join("cleanup");
    let environment = support::environment(&home);
    let first = cleanup_postgres(&environment, url.as_ref(), &out).await;
    assert!(out.join("target.json").is_file(), "{}", first.stderr);
    assert!(!first.stderr.contains(UNSTAMPED), "{}", first.stderr);
    assert!(!first.stderr.contains(MISMATCH), "{}", first.stderr);
    let second = cleanup_postgres(&environment, url.as_ref(), &out).await;
    assert!(!second.stderr.contains(UNSTAMPED), "{}", second.stderr);
    assert!(!second.stderr.contains(MISMATCH), "{}", second.stderr);
    let stamp = out.join("target.json");
    let text = std::fs::read_to_string(&stamp).expect("stamp");
    let edited = stamp_with_a_different_oid(&text);
    assert_ne!(edited, text, "{text}");
    std::fs::write(&stamp, edited).expect("edit");
    let third = cleanup_postgres(&environment, url.as_ref(), &out).await;
    assert_eq!(third.exit, Exit::Failure, "{}", third.stdout);
    assert!(third.stderr.contains(MISMATCH), "{}", third.stderr);
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum CheckpointStamp {
    Postgres { cluster: i64, oid: i64 },
    Sqlite { device: u64, inode: u64 },
}

fn stamp_with_a_different_oid(text: &str) -> String {
    let seen: CheckpointStamp = serde_json::from_str(text).expect("stamp json");
    let CheckpointStamp::Postgres { cluster, oid } = seen else {
        return String::new();
    };
    serde_json::to_string(&CheckpointStamp::Postgres {
        cluster,
        oid: oid.wrapping_add(1),
    })
    .expect("edited stamp")
}

async fn cleanup_postgres(
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
    cli::memory::execute(
        args.as_slice(),
        environment,
        &CancellationToken::new(),
        false,
    )
    .await
}

async fn propose_at(
    home: &tempfile::TempDir,
    database: &std::path::Path,
    out: &std::path::Path,
) -> cli::Report {
    let database = database.display().to_string();
    let out = out.display().to_string();
    let parts = [
        "cleanup",
        "--propose",
        "--sqlite",
        database.as_str(),
        "--out",
        out.as_str(),
        "--model-url",
        "http://127.0.0.1:1/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(
        &args,
        &support::environment(home),
        &CancellationToken::new(),
        false,
    )
    .await
}

async fn dry_run(
    home: &tempfile::TempDir,
    database: &std::path::Path,
    out: &std::path::Path,
) -> cli::Report {
    let database = database.display().to_string();
    let out = out.display().to_string();
    let parts = [
        "cleanup",
        "--dry-run",
        "--sqlite",
        database.as_str(),
        "--out",
        out.as_str(),
        "--model-url",
        "http://127.0.0.1:1/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(
        &args,
        &support::environment(home),
        &CancellationToken::new(),
        false,
    )
    .await
}

async fn propose(environment: &config::Environment, database: &str) -> cli::Report {
    let out = tempfile::tempdir().expect("out folder");
    let parts = [
        "cleanup",
        "--propose",
        "--database",
        database,
        "--out",
        out.path().to_str().expect("utf-8 path"),
        "--model-url",
        "http://127.0.0.1:9/v1",
        "--model",
        "unused",
    ];
    let args: Vec<String> = parts.iter().map(|part| (*part).to_owned()).collect();
    cli::memory::execute(&args, environment, &CancellationToken::new(), false).await
}
