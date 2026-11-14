use migrate::POSTGRES;
use sqlx::PgPool;
use sqlx::Row as _;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const GPUBOX: Uuid = uuid::uuid!("00000000-0000-4000-8000-000000000068");
const KEPT: Uuid = uuid::uuid!("00000000-0000-4000-8000-000000000069");
const SHARED_ENTITY: Uuid = uuid::uuid!("00000000-0000-4000-8000-00000000006a");
const SHARED_WORK: Uuid = uuid::uuid!("00000000-0000-4000-8000-00000000006b");
const TAKEN_ENTITY: Uuid = uuid::uuid!("00000000-0000-4000-8000-00000000006c");
const TAKEN_WORK: Uuid = uuid::uuid!("00000000-0000-4000-8000-00000000006d");
const HOST_ENTITY: Uuid = uuid::uuid!("00000000-0000-4000-8000-00000000006e");

#[sqlx::test(migrations = false)]
async fn entity_page_binding_matches_one_unbound_slug_and_is_idempotent(
    pool: PgPool,
) -> TestResult {
    for migration in POSTGRES.migrations.iter() {
        if !migration.migration_type.is_up_migration() || migration.version >= 68 {
            continue;
        }
        sqlx::raw_sql(migration.sql.as_ref()).execute(&pool).await?;
    }
    seed(&pool).await?;
    apply_binding(&pool).await?;
    assert_bound(&pool).await?;
    sqlx::query("DELETE FROM _rivetos_migrations WHERE name = '0068_entity_page_binding.sql'")
        .execute(&pool)
        .await?;
    apply_binding(&pool).await?;
    assert_bound(&pool).await?;
    Ok(())
}

async fn apply_binding(pool: &PgPool) -> TestResult {
    let binding = POSTGRES
        .migrations
        .iter()
        .find(|migration| migration.version == 68)
        .ok_or("0068 is missing from the migrator")?;
    sqlx::raw_sql(binding.sql.as_ref()).execute(pool).await?;
    Ok(())
}

async fn seed(pool: &PgPool) -> TestResult {
    topic(pool, GPUBOX, "entity", "gpubox", None).await?;
    page(pool, "gpubox", None).await?;
    topic(pool, KEPT, "entity", "kept", None).await?;
    page(pool, "kept-bound", Some(KEPT)).await?;
    page(pool, "kept", None).await?;
    topic(pool, SHARED_WORK, "work", "shared", None).await?;
    topic(pool, SHARED_ENTITY, "entity", "shared", None).await?;
    page(pool, "shared", None).await?;
    topic(pool, TAKEN_WORK, "work", "taken", None).await?;
    topic(pool, TAKEN_ENTITY, "entity", "taken", None).await?;
    page(pool, "taken", Some(TAKEN_WORK)).await?;
    topic(pool, HOST_ENTITY, "entity", "hosts", None).await?;
    page(pool, "hosts", None).await?;
    page(pool, "orphan", None).await?;
    Ok(())
}

async fn assert_bound(pool: &PgPool) -> TestResult {
    assert_eq!(binding(pool, "gpubox").await?, Some(GPUBOX));
    assert_eq!(binding(pool, "kept-bound").await?, Some(KEPT));
    assert_eq!(binding(pool, "kept").await?, None);
    assert_eq!(binding(pool, "shared").await?, None);
    assert_eq!(binding(pool, "taken").await?, Some(TAKEN_WORK));
    assert_eq!(binding(pool, "hosts").await?, None);
    assert_eq!(binding(pool, "orphan").await?, None);
    let recorded = sqlx::query(
        "SELECT count(*)::bigint AS n FROM _rivetos_migrations WHERE name = '0068_entity_page_binding.sql'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(recorded.try_get::<i64, _>("n")?, 1);
    Ok(())
}

async fn topic(
    pool: &PgPool,
    id: Uuid,
    kind: &str,
    slug: &str,
    parent: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
         VALUES ($1, $2, $3, $3, $4, now(), now(), 'owner')",
    )
    .bind(id)
    .bind(kind)
    .bind(slug)
    .bind(parent)
    .execute(pool)
    .await?;
    Ok(())
}

async fn page(pool: &PgPool, slug: &str, topic: Option<Uuid>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO rh_wiki_topics (slug, title, topic_id, owner_user_id) VALUES ($1, $1, $2, 'owner')",
    )
    .bind(slug)
    .bind(topic)
    .execute(pool)
    .await?;
    Ok(())
}

async fn binding(pool: &PgPool, slug: &str) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT topic_id FROM rh_wiki_topics WHERE slug = $1")
        .bind(slug)
        .fetch_one(pool)
        .await
}
