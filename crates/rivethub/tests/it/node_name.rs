use std::path::Path;

use tokio::process::Command;

use crate::process::{TestResult, provider_config, start, stop, write_state};

const CONFIGURED: &str = "rr-configured-node";
const FROM_ENV: &str = "from-env";
const FROM_HOSTNAME_ENV: &str = "from-hostname-env";

fn node_line(root: &str) -> String {
    format!("service node {root} {CONFIGURED} hostname ")
}

fn command(home: &Path, args: &[&str]) -> Command {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"));
    child
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("RIVETHUB_NODE_NAME", FROM_ENV)
        .env("RIVETHUB_NODE_ID", "from-id")
        .env("HOSTNAME", FROM_HOSTNAME_ENV);
    child
}

#[tokio::test]
async fn gateway_healthz_reports_the_configured_node() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    let url = db.served_url().await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!(
                "gateway:\n  host: 127.0.0.1\n  port: {port}\nmesh:\n  node_name: \" {CONFIGURED} \"\n"
            ),
        ),
    )?;
    let started = start(
        command(home.path(), &["gateway"]),
        &["gateway listening on "],
    )
    .await?;
    let health: wire::gateway::HealthzResponse = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(health.ok);
    assert_eq!(health.node, CONFIGURED);
    assert_ne!(health.name, health.node);
    assert_ne!(health.name, FROM_ENV);
    assert_ne!(health.name, FROM_HOSTNAME_ENV);
    let text = stop(started).await?;
    assert!(
        text.contains(&format!("{}{}", node_line("gateway"), health.name)),
        "{text}"
    );
    Ok(())
}
