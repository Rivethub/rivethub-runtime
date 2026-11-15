use std::net::TcpListener;

use super::{DEAD_POSTGRES, TestResult, provider_config, run_rh, stderr, write_state};

#[tokio::test]
async fn worker_check_accepts_an_unreachable_postgres_url() -> TestResult {
    let home = tempfile::tempdir()?;
    let legacy = home.path().join(".rivetos");
    std::fs::create_dir_all(&legacy)?;
    std::fs::write(legacy.join("marker"), "kept")?;
    write_state(
        home.path(),
        &provider_config(home.path(), DEAD_POSTGRES, ""),
    )?;
    let output = run_rh(home.path(), &["worker", "--role", "summarizer", "--check"]).await?;
    let text = stderr(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("worker configuration valid"), "{text}");
    assert_eq!(std::fs::read_to_string(legacy.join("marker"))?, "kept");
    Ok(())
}

#[tokio::test]
async fn mcp_check_does_not_bind_or_connect() -> TestResult {
    let home = tempfile::tempdir()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let extra = format!(
        "transports:\n  mcp:\n    host: 127.0.0.1\n    port: {port}\n    auth_token: token\n"
    );
    write_state(
        home.path(),
        &provider_config(home.path(), DEAD_POSTGRES, &extra),
    )?;
    let output = run_rh(home.path(), &["mcp", "--check"]).await?;
    let text = stderr(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("mcp configuration valid"), "{text}");
    assert_eq!(listener.local_addr()?.port(), port);
    Ok(())
}

#[tokio::test]
async fn mcp_check_refuses_an_empty_host_a_relative_socket_and_a_missing_bearer() -> TestResult {
    let home = tempfile::tempdir()?;
    for (mcp, refusal) in [
        ("host: \"\"\n    auth_token: token", "mcp host is empty"),
        ("socket: relative.sock", "mcp socket path must be absolute"),
        (
            "require_bearer_on_socket: true",
            "mcp socket bearer is required",
        ),
    ] {
        write_state(
            home.path(),
            &provider_config(
                home.path(),
                DEAD_POSTGRES,
                &format!("transports:\n  mcp:\n    {mcp}\n"),
            ),
        )?;
        let output = run_rh(home.path(), &["mcp", "--check"]).await?;
        let text = stderr(&output);
        assert_eq!(output.status.code(), Some(1), "{text}");
        assert!(text.contains(refusal), "{text}");
    }
    Ok(())
}
