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

async fn searching(pool: &PgPool) -> Result<PostgresMemory, Box<dyn std::error::Error>> {
    let scoped = memory_postgres::scoped(PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: testkit::summary_passages::vector(1.0, 0.0),
        }])]));
    Ok(PostgresMemory::new(scoped).with_query_embed(QueryEmbed::new(provider, None, None, None)))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_message_a_chunk_and_a_passage_rank_together(pool: PgPool) -> TestResult {
    let store = searching(&pool).await?;
    Scope::Maintenance
        .run(testkit::mixed_rank::a_message_a_chunk_and_a_passage_rank_together(&store))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_near_tie_parent_outranks_the_passage(pool: PgPool) -> TestResult {
    let store = searching(&pool).await?;
    Scope::Maintenance
        .run(testkit::mixed_rank::a_near_tie_parent_outranks_the_passage(
            &store,
        ))
        .await
}
