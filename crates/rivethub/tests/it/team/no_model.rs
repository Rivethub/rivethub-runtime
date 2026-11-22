use std::time::Duration;

use memory_postgres::MissingModels;
use wire::gateway::memory_models::{ModelHealth, ModelJob};

use super::{DANA, Hub, TestResult};

const NO_MODEL_CONFIG: &str = "memory:\n  postgres:\n    connection_string: postgres://hub@127.0.0.1:1/hub\ngateway:\n  host: 127.0.0.1\n  port: 5975\n";

async fn queued(hub: &Hub) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM graphile_worker._private_jobs")
        .fetch_one(&hub.db.pool())
        .await
}

#[tokio::test]
async fn a_config_without_a_model_passes_the_gateway_check() -> TestResult {
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    tokio::fs::create_dir(&state).await?;
    tokio::fs::write(state.join("config.yaml"), NO_MODEL_CONFIG).await?;
    let env =
        config::Environment::from_vars([(std::ffi::OsStr::new("HOME"), home.path().as_os_str())]);
    let loaded = config::Config::load(
        &env,
        &config::ConfigLocation::Current(state.join("config.yaml")),
    )?;
    assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    assert_eq!(
        rivethub::service::missing_models(&loaded.config, &env),
        MissingModels {
            language: true,
            embeddings: true
        }
    );
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(["gateway", "--check"])
            .env_clear()
            .env("HOME", home.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(!text.contains(config::NO_MODEL_NOTE), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_hub_without_a_model_queues_nothing_it_cannot_run_and_reports_each_role() -> TestResult {
    let hub = Hub::start_with(MissingModels {
        language: true,
        embeddings: true,
    })
    .await?;
    let before = queued(&hub).await?;
    hub.capture(
        DANA,
        "claude-code:00000000-0000-4000-8000-00000000d001",
        &[
            "a message long enough to be chunked and embedded later on",
            "ok",
        ],
    )
    .await?;
    assert_eq!(
        queued(&hub).await?,
        before,
        "capture queued no embedding job"
    );
    let reply = hub.get(DANA, "/api/memory/models").await?;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let jobs: Vec<ModelJob> = serde_json::from_value(reply.body)?;
    let client = reqwest::Client::builder()
        .default_headers(super::headers(DANA)?)
        .build()?;
    crate::store_routes::none_unavailable(&client, &hub.base).await?;
    let pauses = hub.get(DANA, "/api/memory/workers/pauses").await?;
    assert_eq!(pauses.status, 200, "{}", pauses.body);
    assert_eq!(jobs.len(), 6, "one row per role");
    assert!(
        jobs.iter()
            .all(|job| job.health == ModelHealth::Unconfigured
                && job.detail.starts_with("no model is configured")),
        "{jobs:?}"
    );
    hub.stop().await?;

    let modelled = Hub::start().await?;
    let before = queued(&modelled).await?;
    modelled
        .capture(
            DANA,
            "claude-code:00000000-0000-4000-8000-00000000d002",
            &[
                "a message long enough to be chunked and embedded later on",
                "ok",
            ],
        )
        .await?;
    assert!(
        queued(&modelled).await? > before,
        "a hub with a model still queues"
    );
    modelled.stop().await
}
