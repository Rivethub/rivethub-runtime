use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use config::Config;
use config::Environment;
use harness_acp::AcpHarnessAdapter;
use harness_acp::AcpLaunch;
use harness_acp::LaunchTable;
use harness_acp::initialize_handshake;
use kernel::harness::HarnessAdapter;
use kernel::harness::SessionEvent;
use kernel::harness::SessionSpec;
use kernel::harness::TurnInput;
use libtest_mimic::Arguments;
use libtest_mimic::Completion;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use tempfile::tempdir;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

fn main() {
    let args = Arguments::from_args();
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => start(&args, &runtime).exit(),
        Err(error) => libtest_mimic::run(
            &args,
            vec![Trial::test("dev test runtime", move || {
                Err::<(), Failed>(Failed::from(format!("dev test runtime: {error}")))
            })],
        )
        .exit(),
    }
}

fn probe_programs() -> Vec<HarnessId> {
    vec![
        HarnessId::GrokBuild,
        HarnessId::Hermes,
        HarnessId::KimiCode,
        HarnessId::Opencode,
        HarnessId::Omp,
        HarnessId::Dsh,
        HarnessId::ClaudeCode,
        HarnessId::Codex,
    ]
}

fn start(args: &Arguments, runtime: &tokio::runtime::Runtime) -> libtest_mimic::Conclusion {
    let handle = runtime.handle().clone();
    let mut trials = Vec::new();
    for harness in probe_programs() {
        let session_handle = handle.clone();
        trials.push(
            Trial::ignorable_test(format!("real {harness}"), move || {
                session_handle.block_on(with_deadline(run_harness(harness)))
            })
            .with_ignored_flag(true)
            .with_kind("installed harnesses"),
        );
        let handshake_handle = handle.clone();
        trials.push(
            Trial::ignorable_test(format!("initialize {harness}"), move || {
                handshake_handle.block_on(with_deadline(run_initialize(harness)))
            })
            .with_ignored_flag(true)
            .with_kind("needs the real binary"),
        );
    }
    libtest_mimic::run(args, trials)
}

async fn probe(program: &Path) -> Result<bool, Failed> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(program)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|error| {
        Failed::from(format!(
            "{} version probe timed out: {error}",
            program.display()
        ))
    })?;
    match output {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Failed::from(format!(
            "{} version probe failed: {error}",
            program.display()
        ))),
    }
}

fn user_local_binary(program: &Path) -> Option<PathBuf> {
    let name = program.file_name()?;
    if program != Path::new(name) {
        return None;
    }
    let environment = Environment::from_process();
    let home = environment.home().map(PathBuf::from)?;
    let candidate = home.join(".local").join("bin").join(name);
    std::fs::metadata(&candidate).is_ok().then_some(candidate)
}

fn table_with_binary(
    harness: HarnessId,
    binary: &Path,
    cwd: Option<&Path>,
) -> Result<LaunchTable, Failed> {
    let cwd_line = match cwd {
        Some(cwd) => format!("      cwd: '{}'\n", cwd.display()),
        None => String::new(),
    };
    let document = format!(
        "tasks:\n  harnesses:\n    {harness}:\n      binary: '{}'\n{cwd_line}",
        binary.display()
    );
    let config: Config =
        serde_yaml_ng::from_str(&document).map_err(|error| Failed::from(error.to_string()))?;
    Ok(LaunchTable::from_config(&config))
}

fn not_installed(harness: HarnessId, tried: &[PathBuf]) -> String {
    let names: Vec<String> = tried
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    format!(
        "{harness} is not installed on this node: no launcher could be run (tried {})",
        names.join(", ")
    )
}

async fn resolve_installed(
    harness: HarnessId,
    table: LaunchTable,
    configured: &Path,
) -> Result<Result<LaunchTable, String>, Failed> {
    if probe(configured).await? {
        return Ok(Ok(table));
    }
    let Some(user_local) = user_local_binary(configured) else {
        return Ok(Err(not_installed(harness, &[configured.to_owned()])));
    };
    let resolved = table_with_binary(harness, &user_local, None)?;
    if probe(&user_local).await? {
        return Ok(Ok(resolved));
    }
    Ok(Err(not_installed(
        harness,
        &[configured.to_owned(), user_local],
    )))
}

async fn run_harness(harness: HarnessId) -> Result<Completion, Failed> {
    let table = LaunchTable::from_config(&Config::default());
    let configured = table
        .launch(harness)
        .map_err(|error| Failed::from(format!("launch table: {error}")))?
        .command()
        .program
        .clone();
    let table = match resolve_installed(harness, table, &configured).await {
        Ok(Ok(table)) => table,
        Ok(Err(reason)) => return Ok(Completion::ignored_with(reason)),
        Err(failure) => return Err(failure),
    };
    let launch = table
        .launch(harness)
        .map_err(|error| Failed::from(format!("launch table: {error}")))?;
    if let AcpLaunch::Unverified { reason, command } = launch {
        return Ok(Completion::ignored_with(format!(
            "{harness} is not verified: {reason} ({})",
            command.program.display()
        )));
    }
    let cwd = tempdir().map_err(|error| Failed::from(format!("temp dir: {error}")))?;
    let program = launch.command().program.clone();
    let table = table_with_binary(harness, &program, Some(cwd.path()))?;
    let adapter = Arc::new(AcpHarnessAdapter::with_launch_table(table.clone()));
    let spec = SessionSpec {
        session: SessionId::new(harness, "probe"),
        harness,
        model: None,
        effort: None,
        working_dir: Some(cwd.path().to_path_buf()),
    };
    let created = adapter
        .new_session(spec)
        .await
        .map_err(|error| Failed::from(format!("{harness} new session: {error}")))?;
    let mut events = adapter
        .events(&created)
        .await
        .map_err(|error| Failed::from(format!("{harness} events: {error}")))?;
    adapter
        .prompt(
            &created,
            TurnInput::Text("Reply with the single word ok".to_owned()),
        )
        .await
        .map_err(|error| Failed::from(format!("{harness} prompt: {error}")))?;
    adapter
        .cancel(&created)
        .await
        .map_err(|error| Failed::from(format!("{harness} cancel: {error}")))?;
    wait_for_turn_end(&mut events)
        .await
        .map_err(|error| Failed::from(format!("{harness} turn end: {error}")))?;
    Ok(Completion::Completed)
}

async fn run_initialize(harness: HarnessId) -> Result<Completion, Failed> {
    let table = LaunchTable::from_config(&Config::default());
    let launch = table
        .launch(harness)
        .map_err(|error| Failed::from(format!("launch table: {error}")))?;
    let configured = launch.command().clone();
    let table = match resolve_installed(harness, table, &configured.program).await {
        Ok(Ok(table)) => table,
        Ok(Err(reason)) => return Ok(Completion::ignored_with(reason)),
        Err(failure) => return Err(failure),
    };
    let command = table
        .launch(harness)
        .map_err(|error| Failed::from(format!("launch table: {error}")))?
        .command()
        .clone();
    let cwd = tempdir().map_err(|error| Failed::from(format!("temp dir: {error}")))?;
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        initialize_handshake(&command, cwd.path()),
    )
    .await
    .map_err(|error| Failed::from(format!("{harness} initialize timeout: {error}")))?
    .map_err(|error| Failed::from(format!("{harness} initialize: {error}")))?;
    Ok(Completion::Completed)
}

async fn wait_for_turn_end(
    events: &mut tokio::sync::mpsc::Receiver<SessionEvent>,
) -> Result<(), String> {
    loop {
        match events.recv().await {
            Some(SessionEvent::TurnEnded) => return Ok(()),
            Some(_) => {}
            None => return Err("the event stream ended before the turn ended".to_owned()),
        }
    }
}

async fn with_deadline(
    future: impl std::future::Future<Output = Result<Completion, Failed>>,
) -> Result<Completion, Failed> {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .map_err(|error| Failed::from(format!("live ACP test deadline: {error}")))?
}
