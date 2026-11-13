#![cfg(test)]

#[path = "memory_jobs/filing.rs"]
mod filing;
#[path = "memory_jobs/support.rs"]
mod support;
#[path = "memory_jobs/synthesis.rs"]
mod synthesis;
#[path = "memory_jobs/unembedded.rs"]
mod unembedded;

use std::sync::Arc;
use std::time::Duration;

use kernel::queue::{Job, JobKeyMode, JobQueue};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use workers::{EmbedSettings, ProcessRole, Runner, RunnerConfig, SummarySettings, WorkerDeps};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn every_role_claim_is_dispatched_by_the_real_runner(pool: PgPool) -> support::TestResult {
    let queue = Arc::new(support::RecordingQueue::new(&pool).await);
    let memory = Arc::new(PostgresMemory::new(pool));
    let provider = Arc::new(testkit::FakeProvider::default());
    let mut ids = Vec::new();
    for role in ProcessRole::All.selected() {
        assert_eq!(queue.task_names(role), workers::jobs_for(role));
        for name in queue.task_names(role) {
            ids.push(
                queue
                    .enqueue(Job {
                        name,
                        payload: wire::JsonObject::default(),
                        key: None,
                        key_mode: JobKeyMode::Replace,
                        priority: 0,
                        run_at: None,
                        max_attempts: Some(1),
                    })
                    .await?,
            );
        }
    }
    let runner = Runner::new(
        queue.clone(),
        RunnerConfig::new(ProcessRole::All, 4)?,
        WorkerDeps {
            targets: memory.clone(),
            summaries: memory.clone(),
            summarizer: provider.clone(),
            categorizer: workers::RoleModel {
                provider: provider.clone(),
                model: "model".to_owned(),
            },
            embedder: provider.clone(),
            embed: EmbedSettings::standard(),
            summary: SummarySettings::standard("model")?,
            scopes: None,
        },
    )
    .with_memory_jobs(memory.clone())
    .with_unembedded(memory.clone())
    .with_topic_pages(memory.clone(), memory.clone(), memory.clone())
    .with_session_deletion(memory.clone())
    .with_session_topics(workers::SessionTopics {
        store: memory.clone(),
        filing: memory.clone(),
        lineage: Arc::new(testkit::wiki_routes::NoParents),
    })
    .with_tidy(memory.clone())
    .with_entity_builder(workers::entity_builder::EntityBuilderDeps {
        store: memory.clone(),
        provider: provider.clone(),
        model: "model".to_owned(),
        settings: workers::entity_builder::MatcherSettings::default(),
    })
    .with_wiki_builder(workers::wiki_builder::WikiBuilderDeps {
        store: memory.clone(),
        consolidate: Some(memory),
        recompile: None,
        provider,
        model: "model".to_owned(),
        load: None,
    });
    let mut idle = runner.idle();
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let task = tokio::spawn(async move { runner.run(shutdown).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !*idle.borrow_and_update() {
            idle.changed().await.expect("runner running");
        }
    })
    .await?;
    cancel.cancel();
    task.await??;
    for id in ids {
        if let Some(job) = queue.read(id).await? {
            assert_eq!(job.attempts, 1);
            let error = job.last_error.expect("settled failure");
            assert!(
                !error.contains("no handler for job"),
                "{}: {error}",
                job.name
            );
            assert!(
                !error.contains("dependencies were not supplied"),
                "{}: {error}",
                job.name
            );
        }
    }
    Ok(())
}

#[path = "memory_jobs/sweeps.rs"]
mod sweeps;
#[path = "memory_jobs/unchunked.rs"]
mod unchunked;
