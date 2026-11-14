#![cfg(test)]

use std::ffi::OsStr;

use cli::{Cli, Dispatch, Exit, Report};
use config::Environment;
use tokio_util::sync::CancellationToken;

async fn run(home: &std::path::Path, args: &[&str]) -> Report {
    let env = Environment::from_vars([
        (OsStr::new("HOME"), home.as_os_str()),
        (OsStr::new("PATH"), OsStr::new("/nonexistent")),
    ]);
    let cli = Cli::parse_args(
        std::iter::once("rh")
            .chain(args.iter().copied())
            .map(Into::into),
    )
    .expect("parse");
    let Dispatch::Complete(report) = cli::dispatch(cli, &env, &CancellationToken::new()).await
    else {
        panic!("harness commands complete in place");
    };
    report
}

#[tokio::test]
async fn connect_names_a_skipped_part_instead_of_saying_connected() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir(home.path().join(".cursor")).expect("cursor store");
    let report = run(home.path(), &["harness", "connect", "cursor"]).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert!(
        report.stdout.ends_with("connected recall only.\n"),
        "{}",
        report.stdout
    );
    assert!(
        report
            .stderr
            .starts_with("Skipped: cursor has no capture setup rh can write: "),
        "{}",
        report.stderr
    );
    let dry = tempfile::tempdir().expect("home");
    std::fs::create_dir(dry.path().join(".cursor")).expect("cursor store");
    let preview = run(dry.path(), &["harness", "connect", "cursor", "--dry-run"]).await;
    assert!(preview.stdout.ends_with("Dry run: nothing was written.\n"));
    assert!(!dry.path().join(".cursor/mcp.json").exists());
    let only = run(home.path(), &["harness", "connect", "cursor", "--capture"]).await;
    assert_eq!(only.exit, Exit::Failure);
    assert!(!only.stdout.contains("connected"));
}

#[tokio::test]
async fn connect_opencode_installs_the_embedded_capture_plugin() {
    let home = tempfile::tempdir().expect("home");
    std::fs::create_dir_all(home.path().join(".config/opencode")).expect("opencode config");
    let report = run(home.path(), &["harness", "connect", "opencode"]).await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    assert_eq!(report.stderr, "");
    assert!(
        report.stdout.ends_with("\nconnected.\n"),
        "{}",
        report.stdout
    );
    let plugin = std::fs::read_to_string(
        home.path()
            .join(".config/opencode/plugins/rivethub-capture.ts"),
    )
    .expect("plugin");
    assert!(plugin.contains("capture hook --harness opencode"));
    let status = run(home.path(), &["harness", "status"]).await;
    assert!(
        status
            .stdout
            .contains("capture (plugin): plug-in installed"),
        "{}",
        status.stdout
    );
    assert!(
        !status.stdout.contains("capture history unavailable"),
        "{}",
        status.stdout
    );
}

#[tokio::test]
async fn status_reads_the_sqlite_store_and_skips_history_for_a_missing_tool() {
    let home = tempfile::tempdir().expect("home");
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state).expect("state");
    std::fs::create_dir_all(home.path().join(".cursor")).expect("cursor store");
    let database = state.join("memory.sqlite");
    migrate::sqlite(&database).expect("schema");
    std::fs::write(
        state.join("config.yaml"),
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n",
            database.display()
        ),
    )
    .expect("config");
    let env = Environment::from_vars([
        (OsStr::new("HOME"), home.path().as_os_str()),
        (OsStr::new("PATH"), OsStr::new("/nonexistent")),
        (
            OsStr::new(config::POSTGRES_URL_VARIABLE),
            OsStr::new("postgres://not-a-host:1/nope"),
        ),
    ]);
    let empty = dispatch(&env, &["harness", "status"]).await;
    assert_eq!(empty.exit, Exit::Ok, "{}", empty.stderr);
    assert!(
        !empty.stdout.contains("capture history unavailable"),
        "{}",
        empty.stdout
    );
    assert!(
        empty.stdout.contains("nothing captured yet"),
        "{}\nexpected the installed cursor store to say nothing was captured",
        empty.stdout
    );
    assert!(
        !section(&empty.stdout, "Claude Code").contains("nothing captured yet"),
        "{}",
        empty.stdout
    );
    let connection = rusqlite::Connection::open(&database).expect("database");
    connection
        .execute(
            "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
             VALUES (?1, 'cursor:one', 'rivet', ?2, ?2)",
            (
                "0199e2a0-7b3c-7a00-8000-0000000000c1",
                "2026-11-14T15:04:05.000Z",
            ),
        )
        .expect("conversation");
    drop(connection);
    let captured = dispatch(&env, &["harness", "status"]).await;
    assert!(
        captured
            .stdout
            .contains("last capture 2026-11-14T15:04:05Z"),
        "{}",
        captured.stdout
    );
    assert!(
        !section(&captured.stdout, "Claude Code").contains("last capture"),
        "{}",
        captured.stdout
    );
    assert!(
        !captured.stdout.contains("capture history unavailable"),
        "{}",
        captured.stdout
    );
}

fn section(stdout: &str, name: &str) -> String {
    let mut taking = false;
    let mut out = String::new();
    for line in stdout.lines() {
        if !line.starts_with(' ') {
            taking = line.starts_with(name);
        }
        if taking {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

async fn dispatch(env: &Environment, args: &[&str]) -> Report {
    let cli = Cli::parse_args(
        std::iter::once("rh")
            .chain(args.iter().copied())
            .map(Into::into),
    )
    .expect("parse");
    let Dispatch::Complete(report) = cli::dispatch(cli, env, &CancellationToken::new()).await
    else {
        panic!("harness commands complete in place");
    };
    report
}
