use crate::support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cli::ServiceRoot;
use cli::supervise::{Supervision, SupervisionEvent};
use cli::{Cli, Dispatch, Exit};
use kernel::queue::WorkerRole;
use nix::sys::signal::Signal;
use support::{config, environment, home, run};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

const GAPS: &[(&str, &str)] = &[(
    "postgres-worker-roles",
    "A Postgres config has no worker-role list, so rh start does not invent worker children for it.",
)];

const BASE: &str = "\
runtime:\n  \
  workspace: /home/user/work\n  \
  default_agent: main\n\
agents:\n  \
  main:\n    \
    provider: p\n\
providers:\n  \
  p:\n    \
    type: openai-compatible\n    \
    base_url: http://127.0.0.1:1/v1\n    \
    model: m\n";

#[test]
fn skipped_scenarios_are_named_once_with_a_reason() {
    let mut names = std::collections::BTreeSet::new();
    assert_ne!(GAPS, []);
    for (name, reason) in GAPS {
        assert_ne!(*name, "");
        assert_ne!(*reason, "");
        assert!(names.insert(*name), "duplicate scenario {name}");
    }
    assert_eq!(names.len(), GAPS.len());
}

#[tokio::test]
async fn start_refuses_a_missing_config_with_the_contract_text() {
    let home = home();
    let env = environment(&home);
    let report = run(&["start"], &env).await;
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(report.stdout, "");
    assert_eq!(
        report.stderr,
        "Error: No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or use --config <path>\n"
    );
    let help = run(&["--help"], &env).await;
    assert_eq!(help.exit, Exit::Ok);
    assert!(help.stdout.contains("local"));
    let unknown = run(&["local", "--yes"], &env).await;
    assert_eq!(unknown.exit, Exit::Failure);
    assert!(!unknown.stderr.starts_with("Unknown command:"));
    assert!(
        !help
            .stdout
            .contains("Supervise the configured service roots")
    );
}

#[tokio::test]
async fn an_explicit_missing_config_is_an_io_error() {
    let home = home();
    let env = environment(&home);
    let missing = home.path().join("missing.yaml");
    for flag in ["--config", "-c"] {
        let report = run(&["start", flag, missing.to_str().expect("path")], &env).await;
        assert_eq!(report.exit, Exit::Failure, "{flag}");
        assert!(
            report.stderr.starts_with(&format!(
                "Error: Failed to parse config: failed to read config at {}: ",
                missing.display()
            )),
            "{flag}: {}",
            report.stderr
        );
        assert!(!report.stderr.contains("No config found"), "{flag}");
    }
}

#[tokio::test]
async fn another_owner_is_refused_with_the_state_messages() {
    let home = home();
    let env = environment(&home);
    config(&home, "");
    let directory = home.path().join(".rivethub");
    let pid_path = directory.join("rivethub.pid");
    let pid = std::process::id();
    std::fs::write(&pid_path, format!("{pid}\n")).expect("pid");
    let live = run(&["start"], &env).await;
    assert_eq!(live.exit, Exit::Failure);
    assert_eq!(
        live.stderr,
        format!(
            "Error: refusing to move legacy state: live process {pid} owns {shown}\n",
            shown = pid_path.display()
        )
    );

    std::fs::write(&pid_path, "not-a-pid\n").expect("invalid pid");
    let invalid = run(&["start"], &env).await;
    assert_eq!(
        invalid.stderr,
        format!(
            "Error: refusing to move legacy state: invalid owner PID in {shown}\n",
            shown = pid_path.display()
        )
    );

    std::fs::remove_file(&pid_path).expect("remove pid");
    let claim = config::claim_node(&directory, pid).expect("claim");
    let locked = run(&["start"], &env).await;
    assert_eq!(
        locked.stderr,
        format!(
            "Error: refusing to move legacy state: owner lock is held at {shown}\n",
            shown = pid_path.display()
        )
    );
    claim.release().expect("release");
}

#[tokio::test]
async fn a_symlinked_state_directory_is_refused() {
    let home = home();
    let env = environment(&home);
    let real = home.path().join("real-state");
    std::fs::create_dir_all(&real).expect("real state");
    std::fs::write(real.join("config.yaml"), BASE).expect("config");
    let link = home.path().join(".rivethub");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let report = run(&["start"], &env).await;
    assert_eq!(report.exit, Exit::Failure);
    assert_eq!(
        report.stderr,
        format!(
            "Error: refusing to use state directory {shown}: it is a symlink\n",
            shown = link.display()
        )
    );
}

#[tokio::test]
async fn service_roots_keep_their_config_flag() {
    let home = home();
    let env = environment(&home);
    let path = home.path().join("explicit.yaml");
    let gateway = Cli::parse_args(
        ["rh", "gateway", "--config", path.to_str().expect("path")].map(Into::into),
    )
    .expect("gateway");
    let got = require_gateway(cli::dispatch(gateway, &env, &CancellationToken::new()).await)
        .expect("gateway dispatch");
    assert_eq!(got, path);
    let worker = Cli::parse_args(
        [
            "rh",
            "worker",
            "--role",
            "summarizer",
            "-c",
            path.to_str().expect("path"),
        ]
        .map(Into::into),
    )
    .expect("worker");
    let (role, got) = require_worker(cli::dispatch(worker, &env, &CancellationToken::new()).await)
        .expect("worker dispatch");
    assert_eq!(role, workers::ProcessRole::Summarizer);
    assert_eq!(got, path);
    assert!(
        Cli::parse_args(["rh", "agent", "--config", path.to_str().expect("path")].map(Into::into))
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn a_crashing_root_waits_one_second_then_two() {
    let home = home();
    config(&home, "");
    let program = write_script(home.path(), "crash", "exit 0\n").expect("script");
    let shutdown = CancellationToken::new();
    let (task, mut events) = drive(&home, &program, None, &shutdown);
    let (root, arguments) =
        require_spawned(next_event(&mut events).await.expect("spawn")).expect("spawned");
    assert_eq!(root, ServiceRoot::Gateway);
    assert_eq!(arguments, gateway_args(&config_path(&home)));
    assert_eq!(
        wait_delay(&mut events).await.expect("first delay"),
        Duration::from_secs(1)
    );
    let (root, _arguments) =
        require_spawned(next_event(&mut events).await.expect("respawn")).expect("respawned");
    assert_eq!(root, ServiceRoot::Gateway);
    assert_eq!(
        wait_delay(&mut events).await.expect("second delay"),
        Duration::from_secs(2)
    );
    shutdown.cancel();
    let report = task.await.expect("join").expect("shutdown");
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(report.stdout, "");
    assert!(!pid_path(&home).exists());
}

#[tokio::test(start_paused = true)]
async fn backoff_resets_after_a_minute_of_uptime() {
    let home = home();
    config(&home, "");
    let mark = home.path().join("mark");
    let fifo = home.path().join("hold");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .expect("mkfifo");
    let ready = home.path().join("ready");
    let program = write_script(
        home.path(),
        "hold-root",
        &format!(
            "mark={}\nfifo={}\nready={}\nif [ ! -f \"$mark\" ]; then\n  : > \"$mark\"\n  exit 1\nfi\necho ready > \"$ready\"\nread -r _ < \"$fifo\"\nexit 0\n",
            shell_quote(&mark),
            shell_quote(&fifo),
            shell_quote(&ready)
        ),
    )
    .expect("script");
    let shutdown = CancellationToken::new();
    let (task, mut events) = drive(&home, &program, None, &shutdown);
    assert!(matches!(
        next_event(&mut events).await.expect("spawn"),
        SupervisionEvent::Spawned { .. }
    ));
    assert_eq!(
        wait_delay(&mut events).await.expect("first delay"),
        Duration::from_secs(1)
    );
    assert!(matches!(
        next_event(&mut events).await.expect("respawn"),
        SupervisionEvent::Spawned { .. }
    ));
    while !ready.is_file() {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(61)).await;
    let mut writer = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&fifo)
        .await
        .expect("fifo");
    tokio::io::AsyncWriteExt::write_all(&mut writer, b"go\n")
        .await
        .expect("unblock");
    drop(writer);
    assert_eq!(
        wait_delay(&mut events).await.expect("reset delay"),
        Duration::from_secs(1)
    );
    shutdown.cancel();
    task.await.expect("join").expect("shutdown");
}

#[tokio::test]
async fn shutdown_forwards_sigterm_and_removes_the_pid_file() {
    let home = home();
    config(&home, "");
    let marker = home.path().join("termed");
    let ready = home.path().join("term-ready");
    let program = write_script(
        home.path(),
        "term",
        &format!(
            "trap 'echo termed > {}; exit 0' TERM\necho ready > {}\nwhile :; do :; done\n",
            shell_quote(&marker),
            shell_quote(&ready)
        ),
    )
    .expect("script");
    let shutdown = CancellationToken::new();
    let (task, mut events) = drive(&home, &program, None, &shutdown);
    assert!(matches!(
        next_event(&mut events).await.expect("spawn"),
        SupervisionEvent::Spawned { .. }
    ));
    let recorded = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pid_path(&home).is_file() {
                return std::fs::read_to_string(pid_path(&home)).expect("pid text");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pid file");
    assert_eq!(recorded, format!("{}\n", std::process::id()));
    while !ready.is_file() {
        tokio::task::yield_now().await;
    }
    shutdown.cancel();
    let (root, signal) =
        require_stopped(next_event(&mut events).await.expect("stop")).expect("stopped");
    assert_eq!(root, ServiceRoot::Gateway);
    assert_eq!(signal, Signal::SIGTERM);
    let report = task.await.expect("join").expect("shutdown");
    assert_eq!(report.exit, Exit::Ok);
    assert_eq!(report.stdout, "");
    assert!(!pid_path(&home).exists());
    assert_eq!(std::fs::read_to_string(marker).expect("marker"), "termed\n");
}

#[tokio::test(start_paused = true)]
async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
    let home = home();
    config(&home, "");
    let ready = home.path().join("kill-ready");
    let program = write_script(
        home.path(),
        "ignore",
        &format!(
            "trap '' TERM\necho ready > {}\nwhile :; do :; done\n",
            shell_quote(&ready)
        ),
    )
    .expect("script");
    let shutdown = CancellationToken::new();
    let (task, mut events) = drive(&home, &program, None, &shutdown);
    assert!(matches!(
        next_event(&mut events).await.expect("spawn"),
        SupervisionEvent::Spawned { .. }
    ));
    while !ready.is_file() {
        tokio::task::yield_now().await;
    }
    shutdown.cancel();
    let (_root, signal) =
        require_stopped(next_event(&mut events).await.expect("stop")).expect("stopped");
    assert_eq!(signal, Signal::SIGKILL);
    task.await.expect("join").expect("shutdown");
    assert!(!pid_path(&home).exists());
}

#[tokio::test(start_paused = true)]
async fn configured_roots_are_spawned_with_the_resolved_config() {
    let home = home();
    let database = home.path().join("memory.sqlite");
    let wiki = home.path().join("wiki");
    config(
        &home,
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\n    tagging: true\n    wiki_dir: {}\ntransports:\n  mcp:\n    host: 127.0.0.1\n    port: 9\n",
            database.display(),
            wiki.display()
        ),
    );
    let program = write_script(home.path(), "exit", "exit 0\n").expect("script");
    let shutdown = CancellationToken::new();
    let config_file = config_path(&home);
    let (task, mut events) = drive(&home, &program, None, &shutdown);
    let mut seen = Vec::new();
    while seen.len() < 6 {
        match next_event(&mut events).await.expect("event") {
            SupervisionEvent::Spawned { root, arguments } => {
                if !seen
                    .iter()
                    .any(|(found, _): &(ServiceRoot, _)| found == &root)
                {
                    seen.push((root, arguments));
                }
            }
            SupervisionEvent::Waiting { .. } | SupervisionEvent::Stopped { .. } => {}
        }
    }
    shutdown.cancel();
    task.await.expect("join").expect("shutdown");
    let arguments = |root: &ServiceRoot| -> &[OsString] {
        seen.iter()
            .find(|(found, _)| found == root)
            .map(|(_, arguments)| arguments.as_slice())
            .expect("root")
    };
    assert_eq!(arguments(&ServiceRoot::Gateway), gateway_args(&config_file));
    assert_eq!(
        arguments(&ServiceRoot::Worker(WorkerRole::Embedder)),
        worker_args("embedder", &config_file)
    );
    assert_eq!(
        arguments(&ServiceRoot::Worker(WorkerRole::Summarizer)),
        worker_args("summarizer", &config_file)
    );
    assert_eq!(
        arguments(&ServiceRoot::Worker(WorkerRole::Categorizer)),
        worker_args("categorizer", &config_file)
    );
    assert_eq!(
        arguments(&ServiceRoot::Worker(WorkerRole::WikiBuilder)),
        worker_args("wiki-builder", &config_file)
    );
    assert_eq!(
        arguments(&ServiceRoot::Mcp),
        argv(&["mcp", "--config"], &config_file)
    );
}

#[tokio::test(start_paused = true)]
async fn an_empty_service_set_holds_the_pid_until_shutdown() {
    let quiet = support::home();
    config(&quiet, "gateway:\n  enabled: false\n");
    let marker = quiet.path().join("spawned");
    let program = write_script(
        quiet.path(),
        "nope",
        &format!("echo spawned > {}\nexit 1\n", shell_quote(&marker)),
    )
    .expect("script");
    let shutdown = CancellationToken::new();
    let (task, _events) = drive(&quiet, &program, None, &shutdown);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if pid_path(&quiet).is_file() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiet pid");
    assert!(!marker.exists());
    shutdown.cancel();
    let report = task.await.expect("join").expect("quiet shutdown");
    assert_eq!(report.exit, Exit::Ok);
    assert!(!pid_path(&quiet).exists());
    assert!(!marker.exists());
}

#[tokio::test(start_paused = true)]
async fn an_explicit_config_path_is_the_one_children_receive() {
    let home = home();
    let external = home.path().join("elsewhere.yaml");
    std::fs::write(&external, BASE).expect("external config");
    let program = write_script(home.path(), "crash", "exit 0\n").expect("script");
    let shutdown = CancellationToken::new();
    let (task, mut events) = drive(&home, &program, Some(external.clone()), &shutdown);
    let (_root, arguments) =
        require_spawned(next_event(&mut events).await.expect("spawn")).expect("spawned");
    assert_eq!(arguments, gateway_args(&external));
    shutdown.cancel();
    task.await.expect("join").expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn a_program_that_cannot_spawn_still_removes_the_pid_file() {
    let home = home();
    config(&home, "");
    let shutdown = CancellationToken::new();
    let missing = home.path().join("missing-program");
    let (task, _events) = drive(&home, &missing, None, &shutdown);
    let error = task.await.expect("join").expect_err("spawn");
    assert!(error.to_string().contains("No such file"));
    assert!(!pid_path(&home).exists());
}

fn drive(
    home: &tempfile::TempDir,
    program: &Path,
    config: Option<PathBuf>,
    shutdown: &CancellationToken,
) -> (
    tokio::task::JoinHandle<Result<cli::Report, cli::Error>>,
    UnboundedReceiver<SupervisionEvent>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let supervision = Supervision {
        observer: Some(sender),
        program: Some(program.to_path_buf()),
        inherit_output: false,
    };
    let env = environment(home);
    let shutdown = shutdown.clone();
    let task = tokio::spawn(async move { supervision.run(&env, config, &shutdown).await });
    (task, receiver)
}

async fn next_event(
    events: &mut UnboundedReceiver<SupervisionEvent>,
) -> Result<SupervisionEvent, &'static str> {
    events.recv().await.ok_or("observer stayed open")
}

async fn wait_delay(events: &mut UnboundedReceiver<SupervisionEvent>) -> Result<Duration, String> {
    match next_event(events).await.map_err(ToOwned::to_owned)? {
        SupervisionEvent::Waiting { delay, .. } => Ok(delay),
        SupervisionEvent::Spawned { root, arguments } => {
            Err(format!("spawned {root} with {arguments:?}"))
        }
        SupervisionEvent::Stopped { root, signal } => Err(format!("stopped {root} with {signal}")),
    }
}

fn require_spawned(event: SupervisionEvent) -> Result<(ServiceRoot, Vec<OsString>), String> {
    match event {
        SupervisionEvent::Spawned { root, arguments } => Ok((root, arguments)),
        SupervisionEvent::Waiting { root, delay } => Err(format!("waiting {root} for {delay:?}")),
        SupervisionEvent::Stopped { root, signal } => Err(format!("stopped {root} with {signal}")),
    }
}

fn require_stopped(event: SupervisionEvent) -> Result<(ServiceRoot, Signal), String> {
    match event {
        SupervisionEvent::Stopped { root, signal } => Ok((root, signal)),
        SupervisionEvent::Spawned { root, arguments } => {
            Err(format!("spawned {root} with {arguments:?}"))
        }
        SupervisionEvent::Waiting { root, delay } => Err(format!("waiting {root} for {delay:?}")),
    }
}

fn require_gateway(dispatch: Dispatch) -> Result<PathBuf, String> {
    match dispatch {
        Dispatch::Gateway {
            check: false,
            config: Some(path),
        } => Ok(path),
        Dispatch::Gateway { check, config } => {
            Err(format!("gateway check={check} config={config:?}"))
        }
        Dispatch::Worker {
            role,
            check,
            config,
        } => Err(format!("worker {role} check={check} config={config:?}")),
        Dispatch::Mcp { check, config } => Err(format!("mcp check={check} config={config:?}")),
        Dispatch::Local(_) => Err("local".to_owned()),
        Dispatch::Complete(report) => Err(format!("{}{}", report.stdout, report.stderr)),
    }
}

fn require_worker(dispatch: Dispatch) -> Result<(workers::ProcessRole, PathBuf), String> {
    match dispatch {
        Dispatch::Worker {
            role,
            check: false,
            config: Some(path),
        } => Ok((role, path)),
        Dispatch::Worker {
            role,
            check,
            config,
        } => Err(format!("worker {role} check={check} config={config:?}")),
        Dispatch::Gateway { check, config } => {
            Err(format!("gateway check={check} config={config:?}"))
        }
        Dispatch::Mcp { check, config } => Err(format!("mcp check={check} config={config:?}")),
        Dispatch::Local(_) => Err("local".to_owned()),
        Dispatch::Complete(report) => Err(format!("{}{}", report.stdout, report.stderr)),
    }
}

fn write_script(dir: &Path, name: &str, body: &str) -> Result<PathBuf, std::io::Error> {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}"))?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions)?;
    Ok(path)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn config_path(home: &tempfile::TempDir) -> PathBuf {
    home.path().join(".rivethub/config.yaml")
}

fn pid_path(home: &tempfile::TempDir) -> PathBuf {
    home.path().join(".rivethub/rivethub.pid")
}

fn gateway_args(config: &Path) -> Vec<OsString> {
    argv(&["gateway", "--config"], config)
}

fn worker_args(role: &str, config: &Path) -> Vec<OsString> {
    argv(&["worker", "--role", role, "--config"], config)
}

fn argv(parts: &[&str], config: &Path) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = parts.iter().copied().map(OsString::from).collect();
    arguments.push(config.as_os_str().to_owned());
    arguments
}
