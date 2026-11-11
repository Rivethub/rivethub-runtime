use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use config::provider::CliProvider;
use harness_acp::{AcpError, Retry};
use kernel::harness::{HarnessAdapter, HarnessAdapterError, SessionSpec};
use kernel::model::ProviderRecovery;
use kernel::model::ProviderStatus;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::{TestResult, cause, degradation, started, supervisor};

const NATIVE: &str = "20261015_120000_a1b2c3";

fn stalling_launcher(program: &Path, ready: &Path) -> TestResult {
    let quote = |text: &str| text.replace('\'', "'\"'\"'");
    let binary = quote(env!("CARGO_BIN_EXE_fake-acp-agent"));
    let ready = quote(&ready.display().to_string());
    std::fs::write(
        program,
        format!(
            "#!/bin/sh\n[ -e '{ready}' ] || exec sleep 600\nexec '{binary}' --session-id '{NATIVE}' --steps text:Hi\n"
        ),
    )?;
    std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn stalling_provider(
    home: &Path,
    discovery_timeout_ms: u64,
) -> Result<CliProvider, Box<dyn std::error::Error>> {
    let program = home.join("hermes");
    stalling_launcher(&program, &home.join("ready"))?;
    Ok(CliProvider {
        binary: Some(program.display().to_string()),
        cwd: Some(home.into()),
        discovery_timeout_ms: Some(discovery_timeout_ms),
        ..CliProvider::default()
    })
}

fn spec() -> SessionSpec {
    SessionSpec {
        session: SessionId::new(HarnessId::Hermes, NATIVE),
        harness: HarnessId::Hermes,
        model: None,
        effort: None,
        working_dir: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_harness_degrades_fails_fast_and_recovers_without_restart() -> TestResult {
    let home = tempfile::tempdir()?;
    let provider = stalling_provider(home.path(), 1000)?;
    let supervisor = supervisor(Retry {
        first: Duration::from_millis(50),
        ceiling: Duration::from_millis(200),
    });
    let harness = started(
        &config::Config::default(),
        HarnessId::Hermes,
        &provider,
        Vec::new(),
        &supervisor,
    )
    .await;
    let degraded = degradation(&harness).ok_or("a stalled discovery degrades the provider")?;
    assert_eq!(degraded.recovery, ProviderRecovery::Retrying);
    assert!(matches!(
        cause(&degraded),
        Some(AcpError::DiscoveryTimeout { timeout }) if *timeout == Duration::from_millis(1000)
    ));
    let refused = tokio::time::timeout(Duration::from_millis(500), harness.new_session(spec()))
        .await?
        .expect_err("a degraded provider refuses turns");
    let HarnessAdapterError::Failed { message, source } = refused else {
        panic!("typed harness failure expected: {refused}");
    };
    assert_eq!(
        message,
        "provider p is degraded (retrying): ACP discovery timed out after 1000ms"
    );
    assert!(matches!(
        source.downcast_ref::<AcpError>(),
        Some(AcpError::ProviderDegraded { provider, .. }) if provider == "p"
    ));
    std::fs::write(home.path().join("ready"), "")?;
    let mut status = harness.health().status;
    tokio::time::timeout(
        Duration::from_secs(20),
        status.wait_for(|status| matches!(status, ProviderStatus::Ready)),
    )
    .await??;
    let session = harness.new_session(spec()).await?;
    harness.release(&session).await?;
    supervisor.tasks.close();
    tokio::time::timeout(Duration::from_secs(5), supervisor.tasks.wait()).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_a_retry_backoff_ends_the_retry_promptly() -> TestResult {
    let home = tempfile::tempdir()?;
    let provider = stalling_provider(home.path(), 200)?;
    let supervisor = supervisor(Retry {
        first: Duration::from_secs(600),
        ceiling: Duration::from_secs(600),
    });
    let harness = started(
        &config::Config::default(),
        HarnessId::Hermes,
        &provider,
        Vec::new(),
        &supervisor,
    )
    .await;
    assert_eq!(
        degradation(&harness).map(|degraded| degraded.recovery),
        Some(ProviderRecovery::Retrying)
    );
    assert_eq!(supervisor.tasks.len(), 1);
    supervisor.shutdown.cancel();
    supervisor.tasks.close();
    tokio::time::timeout(Duration::from_secs(5), supervisor.tasks.wait()).await?;
    assert_eq!(
        degradation(&harness).map(|degraded| degraded.recovery),
        Some(ProviderRecovery::Retrying)
    );
    Ok(())
}
