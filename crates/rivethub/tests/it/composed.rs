use crate::process;

use std::time::Duration;

use process::TestResult;

#[tokio::test]
async fn check_validates_without_moving_legacy_state_or_connecting_postgres() -> TestResult {
    let home = tempfile::tempdir()?;
    let legacy = home.path().join(".rivetos");
    std::fs::create_dir(&legacy)?;
    std::fs::write(
        legacy.join("config.yaml"),
        format!(
            "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\n",
            home.path().display()
        ),
    )?;
    std::fs::write(legacy.join("rivetos.pid"), std::process::id().to_string())?;
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(["gateway", "--check"])
            .env_clear()
            .env("HOME", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|error| format!("rh gateway --check exceeded the command deadline: {error}"))??;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(legacy.is_dir());
    assert!(!home.path().join(".rivethub").exists());
    let refusal = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .arg("gateway")
            .env_clear()
            .env("HOME", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|error| format!("rh gateway exceeded the command deadline: {error}"))??;
    assert_eq!(refusal.status.code(), Some(1));
    assert!(
        String::from_utf8(refusal.stderr)?.contains("refusing to move legacy state: live process")
    );
    assert!(legacy.is_dir());
    assert!(!home.path().join(".rivethub").exists());
    Ok(())
}
