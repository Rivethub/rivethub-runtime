use std::sync::Arc;

use kernel::model::Embedding;
use kernel::tenancy::Scope;
use memory_postgres::PostgresMemory;
use memory_postgres::QueryEmbed;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testkit::FakeProvider;
use testkit::Script;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn searching(
    pool: &PgPool,
    scripts: usize,
) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    let scoped = memory_postgres::scoped(PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let provider: Arc<dyn kernel::model::ModelProvider> = Arc::new(FakeProvider::new(
        (0..scripts)
            .map(|_| {
                Script::Embed(vec![Embedding {
                    vector: testkit::summary_passages::vector(1.0, 0.0),
                }])
            })
            .collect(),
    ));
    Ok(PostgresMemory::new(scoped).with_query_embed(QueryEmbed::new(provider, None, None, None)))
}

async fn run(
    pool: &PgPool,
    scripts: usize,
    scenario: impl AsyncFnOnce(&PostgresMemory) -> TestResult,
) -> TestResult {
    let store = searching(pool, scripts).await?;
    Scope::Maintenance.run(scenario(&store)).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_trashed_session_loses_its_chunk(pool: PgPool) -> TestResult {
    run(&pool, 1, async |store| {
        testkit::chunk_search::a_trashed_session_loses_its_chunk(store).await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_role_filter_drops_the_other_parents_chunk(pool: PgPool) -> TestResult {
    run(&pool, 1, async |store| {
        testkit::chunk_search::a_role_filter_drops_the_other_parents_chunk(store).await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_time_window_drops_an_older_chunk(pool: PgPool) -> TestResult {
    run(&pool, 1, async |store| {
        testkit::chunk_search::a_time_window_drops_an_older_chunk(store).await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn said_only_drops_a_tool_parents_chunk(pool: PgPool) -> TestResult {
    run(&pool, 1, async |store| {
        testkit::chunk_search::said_only_drops_a_tool_parents_chunk(store).await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_hybrid_tag_keeps_the_tagged_conversations_chunk(pool: PgPool) -> TestResult {
    run(&pool, 1, async |store| {
        testkit::chunk_search::a_hybrid_tag_keeps_the_tagged_conversations_chunk(store).await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn clearing_chunk_vectors_drops_the_hit(pool: PgPool) -> TestResult {
    let store = searching(&pool, 2).await?;
    Scope::Maintenance
        .run(testkit::chunk_search::clearing_chunk_vectors_drops_the_hit(
            &store,
            async || {
                sqlx::query("UPDATE rh_message_chunks SET embedding = NULL")
                    .execute(&pool)
                    .await?;
                Ok(())
            },
        ))
        .await
}
