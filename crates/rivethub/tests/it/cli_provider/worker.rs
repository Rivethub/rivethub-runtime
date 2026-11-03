use std::sync::Arc;
use std::time::Duration;

use kernel::memory::{MemoryStore, SummaryStore};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue};
use rivethub::worker::{WorkerPorts, with_models};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::{TestResult, configured, harness, recorded};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    conversation_id: String,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn composed_summarizer_runs_cli_model_without_embedder(pool: sqlx::PgPool) -> TestResult {
    install_queue(&pool).await?;
    let home = tempfile::tempdir()?;
    let (config, _) = configured(home.path(), config::ProviderType::ClaudeCli, "")?;
    let store = Arc::new(memory_postgres::PostgresMemory::new(pool.clone()));
    let messages = (0..10)
        .map(|index| kernel::memory::MemoryEntry {
            id: None,
            session_id: "cli:summary".into(),
            agent: "main".into(),
            channel: "cli".into(),
            role: wire::message::Role::User,
            content: format!("message {index}"),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: Some(chrono::Utc::now()),
        })
        .collect();
    store.append(&testkit::owner()?, messages).await?;
    let id: String = sqlx::query_scalar(
        "SELECT id::text FROM rh_conversations WHERE session_key = 'cli:summary'",
    )
    .fetch_one(&pool)
    .await?;
    let queue = Arc::new(testkit::fakes::FakeJobQueue::default());
    let job = queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::SummarizeConversation),
            payload: wire::JsonObject::from_serialize(&Payload {
                conversation_id: id.clone(),
            })?,
            key: None,
            key_mode: JobKeyMode::KeepRunAt,
            priority: 0,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    let (events, expected) = recorded()?;
    let harness = Arc::new(harness::Harness::new(events));
    let factory =
        rivethub::models::ModelFactory::default().with_harness("p".into(), harness.clone());
    let shutdown = CancellationToken::new();
    let ports = || WorkerPorts {
        queue: queue.clone(),
        targets: store.clone(),
        summaries: store.clone(),
        wiki: store.clone(),
        consolidate: None,
        recompile: None,
        topics: store.clone(),
        filing: store.clone(),
        pages: store.clone(),
        decisions: store.clone(),
        tidy: store.clone(),
        lineage: Arc::new(capture::listing::StoreSessionLister::new(
            capture::listing::StoreRoots::default(),
        )),
        load: None,
        scopes: None,
    };
    for role in [workers::ProcessRole::Embedder, workers::ProcessRole::All] {
        let refused = with_models(
            role,
            ports(),
            &config,
            &factory,
            &shutdown,
            &kernel::shutdown::Activities::new(),
        );
        assert!(matches!(
            refused,
            Err(rivethub::worker::RootError::Provider(
                rivethub::models::ModelError::Cli(providers::cli::CliError::MissingEmbedder)
            ))
        ));
    }
    let runner = with_models(
        workers::ProcessRole::Summarizer,
        ports(),
        &config,
        &factory,
        &shutdown,
        &kernel::shutdown::Activities::new(),
    )?;
    let mut idle = runner.runner.idle();
    let cancel = shutdown.clone();
    let task = tokio::spawn(async move { runner.run(cancel).await });
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        idle.wait_for(|idle| *idle).await.map(|_| ())
    })
    .await;
    let outcome = queue.read(job).await?;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), task).await???;
    completed??;
    assert!(outcome.is_none(), "job failed: {outcome:?}");
    let window = store
        .conversation(id.parse()?)
        .await?
        .ok_or("conversation")?;
    assert_eq!(window.leaves.len(), 1);
    assert_eq!(window.leaves.first().ok_or("leaf")?.content, expected);
    assert_eq!(
        harness
            .requests
            .lock()
            .await
            .first()
            .ok_or("model request")?
            .model
            .as_deref(),
        Some("agent-model")
    );
    store.close().await?;
    Ok(())
}

async fn install_queue(pool: &sqlx::PgPool) -> TestResult {
    let present: bool = sqlx::query_scalar("SELECT to_regnamespace('graphile_worker') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if present {
        return Ok(());
    }
    let mut setup = pool.begin().await?;
    let search_path: String = sqlx::query_scalar("SHOW search_path")
        .fetch_one(&mut *setup)
        .await?;
    sqlx::raw_sql(include_str!("../../fixtures/graphile_worker.txt"))
        .execute(&mut *setup)
        .await?;
    sqlx::query("SELECT set_config('search_path', $1, false)")
        .bind(search_path)
        .execute(&mut *setup)
        .await?;
    setup.commit().await?;
    Ok(())
}
