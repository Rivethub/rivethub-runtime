use std::sync::Arc;

use kernel::tenancy::Scope;
use memory_postgres::{PostgresMemory, QueryEmbed};
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn seed_passage_parity(pool: &PgPool) -> TestResult {
    use testkit::summary_passages::{
        CONVERSATION, LEGACY, LEGACY_CONVERSATION, OLD, SOURCE, TITLE, at, source_rows, vector,
    };

    for (id, key, title) in [
        (CONVERSATION, "orchard", Some(TITLE)),
        (LEGACY_CONVERSATION, "legacy", None),
    ] {
        sqlx::query(
            "INSERT INTO rh_conversations (id,session_key,agent,channel,owner_user_id,title)
             VALUES ($1,$2,'fixture','cli','alice',$3)",
        )
        .bind(id)
        .bind(key)
        .bind(title)
        .execute(pool)
        .await?;
    }
    for (id, year) in source_rows() {
        sqlx::query("INSERT INTO rh_messages (id,conversation_id,agent,channel,role,content,created_at,owner_user_id)
             VALUES ($1,$2,'fixture','cli','user',$3,$4,'alice')")
            .bind(id).bind(CONVERSATION).bind(SOURCE.repeat(8)).bind(jiff_sqlx::Timestamp::from(at(year)?))
            .execute(pool).await?;
    }
    sqlx::query("INSERT INTO rh_summary_state (conversation_id,input_revision,last_input_at,settled_at,historical_only)
        VALUES ($1,1,$2,$2,false)")
        .bind(CONVERSATION).bind(jiff_sqlx::Timestamp::from(at(2026)?)).execute(pool).await?;
    sqlx::query("INSERT INTO rh_summaries (id,conversation_id,depth,kind,content,message_count,earliest_at,latest_at,created_at,owner_user_id,embedding)
        VALUES ($1,$2,0,'leaf',$3,1,$4,$4,$5,'alice',$6)")
        .bind(LEGACY).bind(LEGACY_CONVERSATION).bind(OLD)
        .bind(jiff_sqlx::Timestamp::from(at(2024)?)).bind(jiff_sqlx::Timestamp::from(at(2099)?))
        .bind(pgvector::HalfVector::from_f32_slice(&vector(0.8, 0.6))).execute(pool).await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_search_and_repair_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, NEW_KEY, provider, search_and_repair};

    seed_passage_parity(&pool).await?;
    let pool = scoped_pool(&pool).await?;
    let wired = |key| {
        PostgresMemory::new(pool.clone())
            .with_summary_index_key(key)
            .with_query_embed(QueryEmbed::new(Arc::new(provider()), None, None, None))
    };
    Scope::Maintenance
        .run(search_and_repair(&wired(KEY), &wired(NEW_KEY)))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_replacement_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, provider, replacement};

    seed_passage_parity(&pool).await?;
    let pool = scoped_pool(&pool).await?;
    let store = PostgresMemory::new(pool)
        .with_summary_index_key(KEY)
        .with_query_embed(QueryEmbed::new(Arc::new(provider()), None, None, None));
    Scope::Maintenance
        .run(replacement(&store, async |tree| {
            store.replace_summary_tree(&tree).await?;
            Ok(())
        }))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_part_limit_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, part_limit, provider};

    seed_passage_parity(&pool).await?;
    let pool = scoped_pool(&pool).await?;
    let store = PostgresMemory::new(pool)
        .with_summary_index_key(KEY)
        .with_query_embed(QueryEmbed::new(Arc::new(provider()), None, None, None));
    Scope::Maintenance.run(part_limit(&store)).await
}

async fn scoped_pool(pool: &PgPool) -> Result<PgPool, sqlx::Error> {
    memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await
}
