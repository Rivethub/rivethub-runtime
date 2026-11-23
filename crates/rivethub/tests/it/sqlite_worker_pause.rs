use std::sync::Arc;
use std::time::Duration;

use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue as _};
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use testkit::FakeProvider;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::wiki_builder::WikiBuilderDeps;
use workers::{EmbedSettings, ProcessRole, Runner, RunnerConfig, SummarySettings, WorkerDeps};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const SOON: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_paused_wiki_builder_claims_nothing_until_it_resumes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(SqliteStore::open(&path)?);
    store.pause(PausableRole::WikiBuilder, "test").await?;
    enqueue(&store, FixedJob::ExtractWiki).await?;
    enqueue(&store, FixedJob::FileConversation).await?;
    let wiki = wiki_runner(Arc::clone(&store))?;
    let categorizer = categorizer_runner(Arc::clone(&store))?;
    let cancel = CancellationToken::new();
    let wiki_cancel = cancel.clone();
    let categorizer_cancel = cancel.clone();
    let wiki_task = tokio::spawn(async move { wiki.run(wiki_cancel).await });
    let categorizer_task = tokio::spawn(async move { categorizer.run(categorizer_cancel).await });
    claimed(&store, "file-conversation").await?;
    assert_eq!(attempts(&store, "extract-wiki").await?, 0);
    store.resume(PausableRole::WikiBuilder).await?;
    claimed(&store, "extract-wiki").await?;
    cancel.cancel();
    tokio::time::timeout(SOON, wiki_task).await???;
    tokio::time::timeout(SOON, categorizer_task).await???;
    Ok(())
}

async fn enqueue(store: &SqliteStore, job: FixedJob) -> TestResult {
    store
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

fn wiki_runner(store: Arc<SqliteStore>) -> TestResult<Runner<SqliteStore>> {
    let model = Arc::new(FakeProvider::new(Vec::new()));
    Ok(Runner::new(
        Arc::clone(&store),
        config(ProcessRole::WikiBuilder)?,
        deps(Arc::clone(&store), Arc::clone(&model))?,
    )
    .with_wiki_builder(WikiBuilderDeps {
        store: store.clone(),
        consolidate: Some(store.clone()),
        recompile: None,
        provider: model,
        model: "fake-chat".to_owned(),
        load: None,
    })
    .with_pauses(store))
}

fn categorizer_runner(store: Arc<SqliteStore>) -> TestResult<Runner<SqliteStore>> {
    let model = Arc::new(FakeProvider::new(Vec::new()));
    Ok(Runner::new(
        Arc::clone(&store),
        config(ProcessRole::Categorizer)?,
        deps(Arc::clone(&store), model)?,
    )
    .with_pauses(store))
}

fn config(role: ProcessRole) -> TestResult<RunnerConfig> {
    let mut config = RunnerConfig::new(role, 1)?;
    config.poll_interval = Duration::from_secs(3600);
    Ok(config)
}

fn deps(store: Arc<SqliteStore>, model: Arc<FakeProvider>) -> TestResult<WorkerDeps> {
    Ok(WorkerDeps {
        targets: store.clone(),
        summaries: store,
        summarizer: model.clone(),
        categorizer: workers::RoleModel {
            provider: model.clone(),
            model: "fake-chat".to_owned(),
        },
        embedder: model,
        embed: EmbedSettings::standard(),
        summary: SummarySettings::standard("fake-chat")?,
        scopes: None,
    })
}

async fn attempts(store: &SqliteStore, task: &str) -> TestResult<i64> {
    let task = task.to_owned();
    let count = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT coalesce(sum(attempts), 0) FROM ros_jobs WHERE task = ?1",
                [task],
                |row| row.get(0),
            )
            .map_err(|source| SqliteError::Query {
                operation: "job attempts",
                source,
            })
        })
        .await?;
    Ok(count)
}

async fn claimed(store: &SqliteStore, task: &str) -> TestResult {
    tokio::time::timeout(SOON, async {
        while attempts(store, task).await? == 0 {
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(sleep, reason = "a claim only updates the job row; it sends no event")
            )]
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await??;
    Ok(())
}
