use std::time::Duration;

#[tokio::test]
async fn check_refuses_an_off_loopback_host_without_tls() -> Result<(), Box<dyn std::error::Error>>
{
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    std::fs::create_dir(&state)?;
    std::fs::write(
        state.join("config.yaml"),
        format!(
            "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    model: m\ngateway:\n  host: 0.0.0.0\n",
            home.path().display()
        ),
    )?;
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(["gateway", "--check"])
            .env_clear()
            .env("HOME", home.path())
            .env("RIVETHUB_SHARED_DIR", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("[rivethub-gateway] refusing to bind 0.0.0.0"),
        "{stderr}"
    );
    Ok(())
}
