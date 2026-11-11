use harness_acp::{AcpCommand, AcpLaunch, LaunchTable};
use kernel::harness::{SessionEvent, SessionSpec};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use wire::harness::HarnessId;
use wire::session_id::SessionId;
pub(super) const SESSION: &str = "ses_fake0123456789abcdefgh";

fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().map_err(|error| error.to_string()))
    {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(error.clone()),
    }
}

pub(super) fn fake_agent_program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-acp-agent"))
}

pub(super) fn launch(steps: &[&str], extra: &[&str]) -> LaunchTable {
    let mut args = vec!["--session-id".to_owned(), SESSION.to_owned()];
    for step in steps {
        args.push("--steps".to_owned());
        args.push((*step).to_owned());
    }
    args.extend(extra.iter().map(|argument| (*argument).to_owned()));
    LaunchTable::from_entries([(
        HarnessId::Opencode,
        AcpLaunch::Native(AcpCommand {
            program: fake_agent_program(),
            args,
            working_dir: None,
            env: Vec::new(),
        }),
    )])
}

pub(super) fn spec(cwd: &Path) -> SessionSpec {
    SessionSpec {
        session: SessionId::new(HarnessId::Opencode, SESSION),
        harness: HarnessId::Opencode,
        model: None,
        effort: None,
        working_dir: Some(cwd.to_path_buf()),
    }
}

pub(super) async fn collect_until_turn_end(
    events: &mut tokio::sync::mpsc::Receiver<SessionEvent>,
) -> Result<Vec<SessionEvent>, String> {
    let mut collected = Vec::new();
    loop {
        match events.recv().await {
            Some(event) => {
                let ended = matches!(event, SessionEvent::TurnEnded);
                collected.push(event);
                if ended {
                    return Ok(collected);
                }
            }
            None => return Err("the stream ended before the turn did".to_owned()),
        }
    }
}

pub(super) fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    runtime().expect("test runtime").block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(30), future)
            .await
            .expect("protocol timeout")
    })
}
