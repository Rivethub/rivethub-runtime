use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tokio::process::Command as TokioCommand;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
async fn import_checks_commit_ids_in_the_configured_wiki() -> Result {
    let home = tempfile::tempdir()?;
    let shared = tempfile::tempdir()?;
    let wiki = home.path().join("wiki");
    let sha = seed(&wiki)?;
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let dump = home.path().join("dump.sql");
    std::fs::write(
        &dump,
        format!(
            "COPY public.ros_summaries (id, content, created_at) FROM stdin;
kept\tkept summary\t2026-10-10 12:00:00+00
\\.
COPY public.ros_wiki_topics (slug, title, git_sha, created_at, updated_at) FROM stdin;
kept\tKept\t{sha}\t2026-10-10 12:00:00+00\t2026-10-10 12:00:00+00
\\.
-- PostgreSQL database dump complete
"
        ),
    )?;
    let config = home.path().join("rivethub.yaml");
    std::fs::write(
        &config,
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    wiki_dir: {}\n",
            database.display(),
            wiki.display()
        ),
    )?;
    let database_arg = database.to_str().ok_or("database path is not utf-8")?;
    let dump_arg = dump.to_str().ok_or("dump path is not utf-8")?;
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        TokioCommand::new(env!("CARGO_BIN_EXE_rh"))
            .args(["memory", "import", "--sqlite", database_arg, dump_arg])
            .env_clear()
            .env("HOME", home.path())
            .env("RIVETHUB_CONFIG", &config)
            .env("RIVETHUB_SHARED_DIR", shared.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stored = topic_sha(&database)?;
    assert_eq!(stored.as_deref(), Some(sha.as_str()));
    Ok(())
}

fn seed(root: &Path) -> Result<String> {
    std::fs::create_dir_all(root)?;
    git(root, &["init", "-b", "main"])?;
    git(root, &["config", "user.email", "wiki@rivetos.dev"])?;
    git(root, &["config", "user.name", "RivetOS Wiki"])?;
    git(root, &["commit", "--allow-empty", "-m", "seed"])?;
    git(root, &["rev-parse", "HEAD"])
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn topic_sha(database: &Path) -> Result<Option<String>> {
    let conn = rusqlite::Connection::open(database)?;
    let sha = conn.query_row(
        "SELECT git_sha FROM ros_wiki_topics WHERE slug = 'kept'",
        [],
        |row| row.get(0),
    )?;
    Ok(sha)
}
