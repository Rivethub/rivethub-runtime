use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use config::provider::CliProvider;
use harness_acp::{AcpError, HeadlessHarness, HeadlessLaunch, Retry, Supervisor, UnverifiedReason};
use kernel::harness::{HarnessAdapter, SessionEvent, SessionSpec, TurnInput};
use kernel::model::ProviderRecovery;
use kernel::model::{Degradation, ProviderStatus};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

#[path = "headless/degraded.rs"]
mod degraded;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn supervisor(retry: Retry) -> Supervisor {
    Supervisor {
        shutdown: CancellationToken::new(),
        tasks: TaskTracker::new(),
        retry,
    }
}

async fn started(
    config: &config::Config,
    harness: HarnessId,
    provider: &CliProvider,
    search: Vec<std::path::PathBuf>,
    supervisor: &Supervisor,
) -> HeadlessHarness {
    HeadlessLaunch::prepare(config, harness, provider, search, "p".into())
        .await
        .start(supervisor.clone())
        .await
}

fn degradation(harness: &HeadlessHarness) -> Option<Degradation> {
    match &*harness.health().status.borrow() {
        ProviderStatus::Ready => None,
        ProviderStatus::Degraded(degradation) => Some(degradation.clone()),
    }
}

fn cause(degradation: &Degradation) -> Option<&AcpError> {
    degradation.cause.downcast_ref::<AcpError>()
}

fn launcher(path: &Path, session: &str) -> TestResult {
    launcher_requiring(path, session, None)
}

fn launcher_requiring(path: &Path, session: &str, executable: Option<&str>) -> TestResult {
    let binary = env!("CARGO_BIN_EXE_fake-acp-agent").replace('\'', "'\"'\"'");
    let guard = executable.map_or_else(String::new, |expected| {
        format!("[ \"$CLAUDE_CODE_EXECUTABLE\" = '{expected}' ] || exit 3\n")
    });
    std::fs::write(
        path,
        format!("#!/bin/sh\n{guard}exec '{binary}' --session-id '{session}' --steps text:Hi\n"),
    )?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[tokio::test]
async fn configured_headless_binary_and_directory_reach_the_acp_process() -> TestResult {
    for harness in [
        HarnessId::ClaudeCode,
        HarnessId::Codex,
        HarnessId::GrokBuild,
        HarnessId::Opencode,
        HarnessId::Hermes,
        HarnessId::KimiCode,
    ] {
        let home = tempfile::tempdir()?;
        let program = home.path().join("launcher with spaces");
        let native = match harness {
            HarnessId::Opencode => "ses_fake0123456789abcdefgh",
            HarnessId::GrokBuild => "0199299a-1a98-7000-8000-000000000001",
            HarnessId::Hermes => "20261015_120000_a1b2c3",
            HarnessId::KimiCode => "session_00000000-0000-4000-8000-000000000001",
            HarnessId::ClaudeCode | HarnessId::Codex => "00000000-0000-4000-8000-000000000001",
            HarnessId::Omp | HarnessId::Dsh | HarnessId::Cursor | HarnessId::Cowork => {
                panic!("not a CLI provider")
            }
        };
        let (config, provider) = headless_setup(harness, home.path(), &program, native)?;
        let supervisor = supervisor(Retry::STANDARD);
        let adapter = started(
            &config,
            harness,
            &provider,
            vec![home.path().into()],
            &supervisor,
        )
        .await;
        assert!(degradation(&adapter).is_none(), "{harness}");
        assert!(supervisor.tasks.is_empty());
        let session = adapter
            .new_session(SessionSpec {
                session: SessionId::new(harness, native),
                harness,
                model: Some("grok-5".into()),
                effort: Some(wire::stream::ThinkingLevel::Low),
                working_dir: None,
            })
            .await?;
        let mut events = adapter.events(&session).await?;
        tokio::time::timeout(
            Duration::from_secs(5),
            adapter.prompt(&session, TurnInput::Text("Say hi.".into())),
        )
        .await??;
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            while let Some(event) = events.recv().await {
                if let SessionEvent::AgentMessageChunk { text: delta } = event {
                    text.push_str(&delta);
                } else if event == SessionEvent::TurnEnded {
                    return text;
                } else {
                    panic!("unexpected event: {event:?}");
                }
            }
            panic!("turn ended without completion");
        })
        .await?;
        assert_eq!(response, "model=grok-5thought_level=lowHi");
        adapter.release(&session).await?;
    }
    Ok(())
}

#[tokio::test]
async fn absent_and_non_executable_programs_fail_before_any_turn() -> TestResult {
    let home = tempfile::tempdir()?;
    let program = home.path().join("launcher");
    for present in [false, true] {
        if present {
            std::fs::write(&program, "not executable")?;
        }
        let provider = CliProvider {
            binary: Some(program.display().to_string()),
            ..CliProvider::default()
        };
        let supervisor = supervisor(Retry::STANDARD);
        let harness = started(
            &config::Config::default(),
            HarnessId::ClaudeCode,
            &provider,
            Vec::new(),
            &supervisor,
        )
        .await;
        let degradation = degradation(&harness).expect("unavailable executable");
        assert_eq!(degradation.recovery, ProviderRecovery::Permanent);
        assert!(matches!(
            cause(&degradation),
            Some(AcpError::UnverifiedLaunch {
                harness: HarnessId::ClaudeCode,
                reason: UnverifiedReason::NotInstalled,
                ..
            })
        ));
        assert!(
            supervisor.tasks.is_empty(),
            "a permanent failure is not retried"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_installed_program_without_acp_is_not_a_launcher() -> TestResult {
    let home = tempfile::tempdir()?;
    let program = home.path().join("launcher");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    for harness in [HarnessId::Hermes, HarnessId::KimiCode] {
        let provider = CliProvider {
            binary: Some(program.display().to_string()),
            cwd: Some(home.path().into()),
            ..CliProvider::default()
        };
        let supervisor = supervisor(Retry::STANDARD);
        let started = started(
            &config::Config::default(),
            harness,
            &provider,
            Vec::new(),
            &supervisor,
        )
        .await;
        let degradation = degradation(&started).expect("unverified invocation");
        assert!(matches!(
            cause(&degradation),
            Some(AcpError::Closed { .. } | AcpError::Protocol { .. } | AcpError::Launch { .. })
        ));
        supervisor.shutdown.cancel();
        supervisor.tasks.close();
        tokio::time::timeout(Duration::from_secs(5), supervisor.tasks.wait()).await?;
    }
    Ok(())
}

fn headless_setup(
    harness: HarnessId,
    home: &Path,
    program: &Path,
    native: &str,
) -> Result<(config::Config, CliProvider), Box<dyn std::error::Error>> {
    match harness {
        HarnessId::ClaudeCode | HarnessId::Codex => {
            let cli = "/opt/vendor/bin/cli with spaces";
            let required = (harness == HarnessId::ClaudeCode).then_some(cli);
            launcher_requiring(program, native, required)?;
            let key = if harness == HarnessId::ClaudeCode {
                "claude-code"
            } else {
                "codex"
            };
            let document = format!(
                "tasks:\n  harnesses:\n    {key}:\n      binary: '{}'\n",
                program.display()
            );
            let config: config::Config = serde_yaml_ng::from_str(&document)?;
            let provider = CliProvider {
                binary: Some(cli.into()),
                cwd: Some(home.into()),
                ..CliProvider::default()
            };
            Ok((config, provider))
        }
        HarnessId::GrokBuild | HarnessId::Opencode | HarnessId::Hermes | HarnessId::KimiCode => {
            launcher(program, native)?;
            let provider = CliProvider {
                binary: Some("launcher with spaces".into()),
                cwd: Some(home.into()),
                ..CliProvider::default()
            };
            Ok((config::Config::default(), provider))
        }
        HarnessId::Omp | HarnessId::Dsh | HarnessId::Cursor | HarnessId::Cowork => {
            Err("not a CLI provider".into())
        }
    }
}
