use async_trait::async_trait;
use memory_sqlite::SqliteStore;
use testkit::conversion_backfill::{self, ConversionSeed};
use uuid::Uuid;

use crate::common::{TestResult, execute, fresh_store};

struct Seed<'a>(&'a SqliteStore);

#[async_trait]
impl ConversionSeed for Seed<'_> {
    async fn convert(&self, conversation: Uuid, state: &str) -> TestResult {
        execute(
            self.0,
            &format!(
                "INSERT INTO rh_summary_conversion_items (conversation_id, last_input_at, estimated_tokens, state)
                 VALUES ('{conversation}', '2026-11-01T00:00:00.000Z', 0, '{state}')
                 ON CONFLICT (conversation_id) DO UPDATE SET state = excluded.state"
            ),
        )
        .await
    }

    async fn release(&self, conversation: Uuid, anchor: Uuid) -> TestResult {
        execute(
            self.0,
            &format!(
                "INSERT INTO ros_summary_deliveries (conversation_id, consumer, message_id, anchor_id)
                 SELECT m.conversation_id, c.consumer, m.id, '{anchor}'
                   FROM ros_messages m CROSS JOIN (SELECT 'wiki' AS consumer UNION ALL SELECT 'entity') c
                  WHERE m.conversation_id = '{conversation}'"
            ),
        )
        .await
    }
}

#[tokio::test]
async fn legacy_leaves_wait_for_their_conversion() -> TestResult {
    let fixture = fresh_store()?;
    conversion_backfill::legacy_leaves_wait_for_their_conversion(
        &fixture.store,
        &Seed(&fixture.store),
    )
    .await
}
