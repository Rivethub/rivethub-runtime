use std::path::Path;
use std::process::Output;
use std::time::Duration;

use tokio::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn rh(home: &Path, build: &Path, args: &[&str]) -> TestResult<Output> {
    let output = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("RIVETHUB_STATIC_DIR", build)
        .env("TOKIO_WORKER_THREADS", "2")
        .current_dir(home)
        .kill_on_drop(true)
        .output();
    Ok(tokio::time::timeout(Duration::from_secs(60), output).await??)
}

#[tokio::test]
async fn a_bundled_web_build_loads_without_a_credential_while_the_api_still_needs_the_token()
-> TestResult {
    let home = tempfile::tempdir()?;
    let build = tempfile::tempdir()?;
    std::fs::write(
        build.path().join("index.html"),
        "<!doctype html>bundled shell",
    )?;
    std::fs::create_dir(build.path().join("assets"))?;
    std::fs::write(build.path().join("assets/app.js"), "console.log('bundled')")?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let started = rh(
        home.path(),
        build.path(),
        &[
            "local",
            "--ensure",
            "--port",
            &port.to_string(),
            "--no-lan",
            "--wait",
            "30",
        ],
    )
    .await?;
    assert!(started.status.success(), "{started:?}");
    let origin = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder().no_proxy().build()?;
    let shell = http.get(format!("{origin}/")).send().await?;
    assert_eq!(shell.status(), 200);
    assert!(shell.text().await?.contains("bundled shell"));
    let asset = http.get(format!("{origin}/assets/app.js")).send().await?;
    assert_eq!(asset.status(), 200);
    let api = http
        .get(format!("{origin}/api/memory/search?q=x"))
        .send()
        .await?;
    assert_eq!(api.status(), 401);
    let token = std::fs::read_to_string(home.path().join(".rivethub/local-owner.token"))?;
    let authorized = http
        .get(format!("{origin}/api/memory/search?q=x"))
        .bearer_auth(token.trim())
        .send()
        .await?;
    assert_ne!(authorized.status(), 401);
    let stopped = rh(home.path(), build.path(), &["stop"]).await?;
    assert!(stopped.status.success(), "{stopped:?}");
    Ok(())
}
