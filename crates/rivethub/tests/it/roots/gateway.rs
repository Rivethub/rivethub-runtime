use std::path::Path;

use tokio::process::Command;

use crate::process::{Started, start, stop};

use super::{TestResult, provider_config, write_state};

async fn start_gateway(home: &Path) -> TestResult<Started> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rh"));
    command.arg("gateway").env_clear().env("HOME", home);
    start(command, &["gateway listening on "]).await
}

async fn configure(home: &Path, gateway: &str) -> TestResult<crate::pgdb::TestDb> {
    let db = crate::pgdb::TestDb::bare().await?;
    migrate::postgres_pool(&db.pool()).await?;
    let url = db.served_url().await?;
    write_state(home, &provider_config(home, &url, gateway))?;
    Ok(db)
}

#[tokio::test]
async fn a_retired_certificate_authority_section_only_warns_and_the_gateway_serves() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let _db = configure(
        home.path(),
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {port}\n  certificate_authority:\n    url: https://ca.example.test\n    root: /missing/root.pem\n"
        ),
    )
    .await?;
    let text = stop(start_gateway(home.path()).await?).await?;
    assert!(
        text.contains("\"gateway.certificate_authority\" is no longer used"),
        "{text}"
    );
    Ok(())
}
