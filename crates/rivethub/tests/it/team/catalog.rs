use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, Environment};
use rivethub::composition::Composition;
use tokio_util::sync::CancellationToken;

use super::{ALEX, Hub, PHIL, SPACE, TestResult, users};

#[tokio::test]
async fn a_second_node_with_a_stale_users_file_stops_instead_of_rewriting_memberships() -> TestResult
{
    let hub = Hub::start().await?;
    let home = tempfile::tempdir()?;
    let users_file = home.path().join("users.json");
    let mut stale = users();
    if let Some(space) = stale
        .spaces
        .as_mut()
        .and_then(|spaces| spaces.get_mut(SPACE))
    {
        space.members = vec![PHIL.to_owned(), ALEX.to_owned()];
    }
    tokio::fs::write(&users_file, serde_json::to_vec(&stale)?).await?;
    let env = Environment::from_vars([
        (
            OsStr::new(config::POSTGRES_URL_VARIABLE),
            OsStr::new(&hub.url),
        ),
        (OsStr::new("HOME"), home.path().as_os_str()),
        (OsStr::new("RIVETHUB_SHARED_DIR"), home.path().as_os_str()),
        (OsStr::new("RIVETHUB_NODE_NAME"), OsStr::new("second-node")),
    ]);
    let config: Config = serde_yaml_ng::from_str(&format!(
        "gateway:\n  host: 127.0.0.1\n  port: 0\n  users_file: {}\n",
        users_file.display()
    ))?;
    let memory = Arc::new(memory_postgres::PostgresMemory::connect(&hub.url).await?);
    let shutdown = CancellationToken::new();
    let mut second = Composition::on_postgres(
        &config,
        &env,
        &memory,
        &shutdown,
        &kernel::shutdown::Activities::new(),
    )
    .await?;
    let failed = tokio::time::timeout(Duration::from_secs(20), second.failed()).await?;
    let message = failed
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("kept by node"), "{message}");
    let members: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_space_members")
        .fetch_one(&hub.db.pool())
        .await?;
    assert_eq!(members, 3, "the stale node removed nobody");
    shutdown.cancel();
    second.close().await?;
    hub.stop().await
}
