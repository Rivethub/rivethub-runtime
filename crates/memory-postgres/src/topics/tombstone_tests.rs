use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn replaying_a_topic_delete_keeps_the_tombstone_owner(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO rh_topics (id, kind, slug, name, owner_user_id, created_at, updated_at)
         VALUES (gen_random_uuid(), 'work', 'retired', 'Retired', 'alice', now(), now())
         RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    let mut topic = super::super::rows::by_id(&mut tx, kernel::topics::TopicId::from_uuid(id))
        .await?
        .ok_or("missing test topic")?;
    super::remove(&mut tx, &topic).await?;
    super::remove(&mut tx, &topic).await?;
    assert_eq!(owner(&mut tx).await?.as_deref(), Some("alice"));
    topic.owner = None;
    super::remove(&mut tx, &topic).await?;
    assert_eq!(owner(&mut tx).await?.as_deref(), Some("alice"));
    Ok(())
}

async fn owner(tx: &mut sqlx::PgConnection) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT owner_user_id FROM rh_topic_tombstones WHERE kind = 'work' AND slug = 'retired'",
    )
    .fetch_one(tx)
    .await
}
