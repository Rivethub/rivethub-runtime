mod checks;
mod gateway;
mod mcp;
mod start;
mod worker;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const DEAD_POSTGRES: &str = "postgres://127.0.0.1:1/none";

const OMITTED: &[(&str, &str)] = &[
    (
        "internet_search",
        "B7: harness owns web tools; removed by design",
    ),
    ("web_fetch", "B7: harness owns web tools; removed by design"),
    ("echo", "Product decision: not a sidecar tool"),
    (
        "skill_list",
        "Product decision: skills are not sidecar tools",
    ),
    (
        "skill_manage",
        "Product decision: skills are not sidecar tools",
    ),
    (
        "delegate_task",
        "Delegation left rh for T3 Code (CONTRACT note 132)",
    ),
    (
        "list_agents",
        "Delegation left rh for T3 Code (CONTRACT note 132)",
    ),
    ("categorizer", "categorizer has no handler in this build"),
    ("wiki-builder", "wiki-builder has no handler in this build"),
];

fn provider_config(home: &Path, postgres: impl std::fmt::Display, extra: &str) -> String {
    provider_at(home, postgres, "http://127.0.0.1:1", extra)
}

fn provider_at(home: &Path, postgres: impl std::fmt::Display, base: &str, extra: &str) -> String {
    format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: m\nproviders:\n  p:\n    type: openai-compatible\n    base_url: {base}\n    model: m\nmemory:\n  postgres:\n    connection_string: {postgres}\n{extra}",
        home.display()
    )
}

fn write_state(home: &Path, body: &str) -> TestResult {
    let state = home.join(".rivethub");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("config.yaml"), body)?;
    Ok(())
}

async fn run_rh(home: &Path, args: &[&str]) -> TestResult<std::process::Output> {
    Ok(tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(args)
            .env_clear()
            .env("HOME", home)
            .output(),
    )
    .await??)
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn signal_process(pid: u32, signal: nix::sys::signal::Signal) -> TestResult {
    let pid = i32::try_from(pid)?;
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal)?;
    Ok(())
}

fn terminate(pid: u32) -> TestResult {
    signal_process(pid, nix::sys::signal::Signal::SIGTERM)
}

fn interrupt(pid: u32) -> TestResult {
    signal_process(pid, nix::sys::signal::Signal::SIGINT)
}

struct Blackhole {
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Blackhole {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn blackhole() -> TestResult<Blackhole> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0_u8; 1024];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            });
        }
    });
    Ok(Blackhole { address, task })
}

async fn install_queue(pool: &PgPool) -> Result<(), sqlx::Error> {
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('graphile_worker._private_jobs') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if present {
        return Ok(());
    }
    let mut transaction = pool.begin().await?;
    let path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::raw_sql(include_str!("../../fixtures/graphile_worker.txt"))
        .execute(&mut *transaction)
        .await?;
    sqlx::query("SELECT set_config('search_path', $1, false)")
        .bind(path)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await
}

fn readme_skips(text: &str) -> BTreeMap<String, String> {
    let mut skips = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("- `") else {
            continue;
        };
        let Some((name, reason)) = rest.split_once("` — ") else {
            continue;
        };
        skips.insert(name.to_owned(), reason.trim().to_owned());
    }
    skips
}

#[test]
fn readme_names_every_omitted_scenario() {
    let readme =
        std::fs::read_to_string(testkit::recording("roots/README.md")).expect("roots README");
    let parsed = readme_skips(&readme);
    let expected: BTreeMap<_, _> = OMITTED
        .iter()
        .copied()
        .map(|(name, reason)| (name.to_owned(), reason.to_owned()))
        .collect();
    assert_eq!(parsed, expected);
    for reason in parsed.values() {
        assert_ne!(reason, "");
    }
}
