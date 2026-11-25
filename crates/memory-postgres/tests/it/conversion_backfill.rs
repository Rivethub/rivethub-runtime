use async_trait::async_trait;
use kernel::tenancy::Scope;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use testkit::conversion_backfill::{self, ConversionSeed};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Seed(PgPool);

#[async_trait]
impl ConversionSeed for Seed {
    async fn convert(&self, conversation: Uuid, state: &str) -> TestResult {
        sqlx::query(
            "INSERT INTO rh_summary_conversion_items (conversation_id, last_input_at, estimated_tokens, state)
             VALUES ($1, now(), 0, $2)
             ON CONFLICT (conversation_id) DO UPDATE SET state = EXCLUDED.state",
        )
        .bind(conversation)
        .bind(state)
        .execute(&self.0)
        .await?;
        Ok(())
    }

    async fn release(&self, conversation: Uuid, anchor: Uuid) -> TestResult {
        sqlx::query(
            "INSERT INTO rh_summary_deliveries (conversation_id, consumer, message_id, anchor_id)
             SELECT m.conversation_id, c.consumer, m.id, $2
               FROM rh_messages m CROSS JOIN (VALUES ('wiki'), ('entity')) AS c (consumer)
              WHERE m.conversation_id = $1",
        )
        .bind(conversation)
        .bind(anchor)
        .execute(&self.0)
        .await?;
        Ok(())
    }
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn legacy_leaves_wait_for_their_conversion(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    Scope::Maintenance
        .run(conversion_backfill::legacy_leaves_wait_for_their_conversion(&store, &Seed(pool)))
        .await
}
