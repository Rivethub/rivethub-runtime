use jiff::Timestamp;
use std::sync::Arc;

use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use workers::FilingEnv;
use workers::FilingPlan;
use workers::SessionTopics;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_subagent_key_follows_its_parent_and_is_not_filed(pool: PgPool) -> TestResult {
    let memory = Arc::new(PostgresMemory::new(pool));
    let conversation = testkit::filing::capture(
        memory.as_ref(),
        testkit::wiki_lineage::SUBAGENT_KEY,
        "claude-code",
        "Billing check",
        None,
    )
    .await?;
    let topics = SessionTopics {
        store: memory.clone(),
        filing: memory.clone(),
        lineage: Arc::new(testkit::wiki_routes::NoParents),
    };
    let provider = testkit::FakeProvider::default();
    let plan = workers::plan_filing(
        &FilingEnv {
            topics: &topics,
            provider: &provider,
            model: "model",
            now: Timestamp::now(),
            cancel: &CancellationToken::new(),
        },
        conversation,
    )
    .await?;
    assert_eq!(plan, FilingPlan::Inherits);
    assert_eq!(provider.chat_requests().await.len(), 0);
    Ok(())
}
