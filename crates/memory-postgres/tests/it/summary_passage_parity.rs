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
    use testkit::summary_passages::{KEY, replacement};

    seed_passage_parity(&pool).await?;
    let pool = scoped_pool(&pool).await?;
    let store = searchable_store(pool, KEY);
    Scope::Maintenance
        .run(replacement(&store, async |tree| {
            store.replace_summary_tree(&tree).await?;
            Ok(())
        }))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_part_limit_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, part_limit};

    seed_passage_parity(&pool).await?;
    let pool = scoped_pool(&pool).await?;
    let store = searchable_store(pool, KEY);
    Scope::Maintenance.run(part_limit(&store)).await
}

async fn scoped_pool(pool: &PgPool) -> Result<PgPool, sqlx::Error> {
    memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await
}

fn searchable_store(pool: PgPool, key: [u8; 32]) -> PostgresMemory {
    PostgresMemory::new(pool)
        .with_summary_index_key(key)
        .with_query_embed(QueryEmbed::new(
            Arc::new(testkit::summary_passages::provider()),
            None,
            None,
            None,
        ))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_hollow_section_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, hollow_section};

    seed_passage_parity(&pool).await?;
    let store = searchable_store(scoped_pool(&pool).await?, KEY);
    Scope::Maintenance
        .run(hollow_section(&store, async || {
            sqlx::raw_sql(
                "UPDATE rh_summaries SET source_first = 9, source_last = 9 WHERE kind = 'section';
                 CREATE FUNCTION test_hollow_section() RETURNS trigger LANGUAGE plpgsql AS $fn$
                 BEGIN
                     UPDATE rh_summaries SET source_first = 9, source_last = 9 WHERE id = NEW.id;
                     RETURN NEW;
                 END
                 $fn$;
                 CREATE TRIGGER test_hollow_section AFTER INSERT ON rh_summaries
                     FOR EACH ROW WHEN (NEW.kind = 'section') EXECUTE FUNCTION test_hollow_section();",
            ).execute(&pool).await?;
            Ok(())
        }, async |tree| {
            store.replace_summary_tree(&tree).await?;
            Ok(())
        }))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_partial_index_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, SESSION, partial_index};

    seed_passage_parity(&pool).await?;
    let store = searchable_store(scoped_pool(&pool).await?, KEY);
    Scope::Maintenance
        .run(partial_index(&store, async || {
            sqlx::query("UPDATE rh_summary_passages SET embedding = NULL WHERE record_id = $1 AND ordinal = 1")
                .bind(SESSION).execute(&pool).await?;
            Ok(sqlx::query_scalar("SELECT index_needs_repair FROM rh_summaries WHERE id = $1")
                .bind(SESSION).fetch_one(&pool).await?)
        }))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_inverted_times_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, SESSION, inverted_times};

    seed_passage_parity(&pool).await?;
    let store = searchable_store(scoped_pool(&pool).await?, KEY);
    Scope::Maintenance
        .run(inverted_times(&store, async || {
            let insert = sqlx::query(
                "INSERT INTO rh_summary_passages
                 (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key)
                 SELECT gen_random_uuid(), record_id, 2, char_start, char_end, content, latest_at, earliest_at, index_key
                 FROM rh_summary_passages WHERE record_id = $1 AND ordinal = 0",
            ).bind(SESSION).execute(&pool).await;
            let update = sqlx::query(
                "UPDATE rh_summary_passages SET latest_at = '2024-01-01T01:00:00+02:00'
                 WHERE record_id = $1 AND ordinal = 0",
            ).bind(SESSION).execute(&pool).await;
            Ok((is_check_violation(insert), is_check_violation(update)))
        }))
        .await
}

fn is_check_violation(result: Result<sqlx::postgres::PgQueryResult, sqlx::Error>) -> bool {
    matches!(result, Err(sqlx::Error::Database(error)) if error.is_check_violation())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_passage_window_growth_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, PassageRow, window_growth};

    seed_passage_parity(&pool).await?;
    let scoped = scoped_pool(&pool).await?;
    let store = searchable_store(scoped.clone(), KEY);
    Scope::Maintenance
        .run(window_growth(&store, async |tree, key| {
            searchable_store(scoped.clone(), key).replace_summary_tree(&tree).await?;
            Ok(())
        }, async || {
            let rows = sqlx::query_as(
                "SELECT id, content, embedding IS NOT NULL FROM rh_summary_passages ORDER BY ordinal",
            ).fetch_all(&pool).await?;
            Ok(rows.into_iter().map(|(id, content, embedded)| PassageRow { id, content, embedded }).collect())
        }))
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_search_tags_parity(pool: PgPool) -> TestResult {
    use testkit::summary_passages::{KEY, summary_tags};

    seed_passage_parity(&pool).await?;
    let store = searchable_store(scoped_pool(&pool).await?, KEY);
    Scope::Maintenance.run(summary_tags(&store)).await
}
