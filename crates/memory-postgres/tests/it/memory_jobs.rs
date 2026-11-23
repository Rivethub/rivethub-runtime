use std::sync::Arc;

use kernel::model::Embedding;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use memory_postgres::QueryEmbed;
use sqlx::PgPool;
use testkit::FakeProvider;
use testkit::Script;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_phrase_inside_a_long_message_outranks_its_parent(pool: PgPool) -> TestResult {
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }])]));
    let memory =
        PostgresMemory::new(pool).with_query_embed(QueryEmbed::new(provider, None, None, None));
    testkit::memory_jobs::a_phrase_inside_a_long_message_outranks_its_parent(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_nearest_chunk_pool_keeps_one_chunk_per_message(pool: PgPool) -> TestResult {
    let provider: Arc<dyn kernel::model::ModelProvider> = Arc::new(FakeProvider::new(vec![
        Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }]),
        Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }]),
    ]));
    let memory =
        PostgresMemory::new(pool).with_query_embed(QueryEmbed::new(provider, None, None, None));
    testkit::memory_jobs::the_nearest_chunk_pool_keeps_one_chunk_per_message(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_parent_outside_the_cap_uses_the_chunk_score(pool: PgPool) -> TestResult {
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }])]));
    let memory =
        PostgresMemory::new(pool).with_query_embed(QueryEmbed::new(provider, None, None, None));
    testkit::memory_jobs::a_parent_outside_the_cap_uses_the_chunk_score(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_chunk_write_rejects_a_vector_wider_than_the_message(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::memory_jobs::a_chunk_write_rejects_a_vector_wider_than_the_message(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unchunked_counts_trimmed_characters_and_caps_tool_output(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::memory_jobs::unchunked_counts_trimmed_characters_and_caps_tool_output(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn chunks_are_written_once_and_an_unknown_message_is_skipped(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::memory_jobs::chunks_are_written_once_and_an_unknown_message_is_skipped(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_tool_sentence_replaces_only_matching_content(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::memory_jobs::a_tool_sentence_replaces_only_matching_content(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn encoded_tool_arguments_round_trip_and_reject_bad_json(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let id = testkit::memory_jobs::remember_tool_call(&memory).await?;
    plant(&pool, id, r#"{"path":"notes.txt"}"#).await?;
    testkit::memory_jobs::tool_arguments_match(&memory, id, "notes.txt").await?;
    plant(&pool, id, "not-json").await?;
    testkit::memory_jobs::tool_arguments_are_rejected(&memory, id).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_unembedded_chunk_is_not_healed_and_clears_once_written(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    testkit::memory_jobs::an_unembedded_chunk_is_not_healed_and_clears_once_written(&memory).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_non_heartbeat_leaf_is_the_only_missing_wiki(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::a_non_heartbeat_leaf_is_the_only_missing_wiki(&memory, &queue).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_fresh_dead_wiki_job_is_not_revived(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::a_fresh_dead_wiki_job_is_not_revived(&memory, &queue).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_empty_assistant_tool_call_is_backfilled_once(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::an_empty_assistant_tool_call_is_backfilled_once(
        &memory,
        &queue,
        async move |limit, dry| {
            jobs.backfill_tool_synth(limit, dry)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        },
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_exhausted_wiki_extraction_stays_suppressed(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::an_exhausted_wiki_extraction_stays_suppressed(&memory, &queue).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_exhausted_tool_synthesis_is_backfilled_again(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::an_exhausted_tool_synthesis_is_backfilled_again(
        &memory,
        &queue,
        async move |limit, dry| {
            jobs.backfill_tool_synth(limit, dry)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        },
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_chunk_write_enqueues_its_embedding(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::a_chunk_write_enqueues_its_embedding(&memory, &queue).await
}

async fn plant(pool: &PgPool, id: Uuid, args: &str) -> TestResult {
    sqlx::query("UPDATE rh_messages SET tool_args = to_jsonb($2::text) WHERE id = $1")
        .bind(id)
        .bind(args)
        .execute(pool)
        .await?;
    Ok(())
}
