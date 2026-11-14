use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

pub(super) async fn bind_page(
    tx: &mut Transaction<'_, Postgres>,
    topic: Uuid,
    slug: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE rh_wiki_topics SET topic_id = NULL WHERE topic_id = $1 AND slug <> $2")
        .bind(topic)
        .bind(slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_wiki_topics SET topic_id = $1 WHERE slug = $2")
        .bind(topic)
        .bind(slug)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
