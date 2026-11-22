use std::sync::Arc;
use std::time::Duration;

use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue as _};
use memory_postgres::{PostgresMemory, PostgresQueue};
use sqlx::PgPool;
use testkit::FakeProvider;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::entity_builder::{EntityBuilderDeps, MatcherSettings};
use workers::wiki_builder::WikiBuilderDeps;
use workers::{EmbedSettings, ProcessRole, Runner, RunnerConfig, SummarySettings, WorkerDeps};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const SOON: Duration = Duration::from_secs(10);

async fn queue(queue: &PostgresQueue, job: FixedJob) -> TestResult {
    queue
        .enqueue(Job {
            name: JobName::Fixed(job),
            payload: JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: Some(5),
        })
        .await?;
    Ok(())
}

async fn attempts(pool: &PgPool, job: FixedJob) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT coalesce(sum(j.attempts), 0)::bigint FROM graphile_worker._private_jobs j
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id WHERE t.identifier = $1",
    )
    .bind(job.to_string())
    .fetch_one(pool)
    .await
}

async fn claimed_soon(pool: &PgPool, job: FixedJob) -> TestResult {
    tokio::time::timeout(SOON, async {
        while attempts(pool, job).await? == 0 {
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(sleep, reason = "a claim only updates the job row; it sends no event")
            )]
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_, sqlx::Error>(())
    })
    .await??;
    Ok(())
}

fn runner(
    queue: Arc<PostgresQueue>,
    store: &Arc<PostgresMemory>,
) -> TestResult<Runner<PostgresQueue>> {
    let model = Arc::new(FakeProvider::new(Vec::new()));
    let mut config = RunnerConfig::new(ProcessRole::All, 2)?;
    config.poll_interval = Duration::from_secs(3600);
    Ok(Runner::new(
        queue,
        config,
        WorkerDeps {
            targets: store.clone(),
            summaries: store.clone(),
            summarizer: model.clone(),
            categorizer: workers::RoleModel {
                provider: model.clone(),
                model: "fake-chat".to_owned(),
            },
            embedder: model.clone(),
            embed: EmbedSettings::standard(),
            summary: SummarySettings::standard("fake-chat")?,
            scopes: None,
        },
    )
    .with_wiki_builder(WikiBuilderDeps {
        store: store.clone(),
        consolidate: None,
        recompile: None,
        provider: model.clone(),
        model: "fake-chat".to_owned(),
        load: None,
    })
    .with_entity_builder(EntityBuilderDeps {
        store: store.clone(),
        provider: model,
        model: "fake-chat".to_owned(),
        settings: MatcherSettings::default(),
    })
    .with_pauses(store.clone()))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_paused_role_claims_nothing_until_it_resumes_and_other_roles_keep_claiming(
    pool: PgPool,
) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let jobs = Arc::new(PostgresQueue::connect(pool.clone()).await?);
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    store.pause(PausableRole::EntityBuilder, "test").await?;
    queue(&jobs, FixedJob::ExtractEntities).await?;
    queue(&jobs, FixedJob::FileConversation).await?;

    let runner = runner(Arc::clone(&jobs), &store)?;
    let mut idle = runner.idle();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move { runner.run(task_cancel).await });

    claimed_soon(&pool, FixedJob::FileConversation).await?;
    tokio::time::timeout(SOON, idle.wait_for(|idle| *idle)).await??;
    assert_eq!(
        attempts(&pool, FixedJob::ExtractEntities).await?,
        0,
        "the paused entity builder claimed a job"
    );

    store.resume(PausableRole::EntityBuilder).await?;
    claimed_soon(&pool, FixedJob::ExtractEntities).await?;

    cancel.cancel();
    tokio::time::timeout(SOON, task).await???;
    Ok(())
}
