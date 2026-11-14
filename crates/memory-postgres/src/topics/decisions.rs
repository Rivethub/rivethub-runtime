use std::collections::BTreeSet;
use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::topics::Decision;
use kernel::topics::DecisionId;
use kernel::topics::DecisionStatus;
use kernel::topics::DecisionStore;
use kernel::topics::DecisionText;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use uuid::Uuid;
use wire::session_id::SessionId;

use super::rows::failed;
use super::rows::stored;
use super::write::enqueue_syncs;
use crate::PostgresMemory;

struct DecisionRow {
    id: Uuid,
    topic_id: Uuid,
    text: String,
    status: String,
    superseded_by: Option<Uuid>,
    decided_by: String,
    session_id: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn decision(row: DecisionRow) -> Result<Decision, TopicError> {
    Ok(Decision {
        id: DecisionId::from_uuid(row.id),
        topic: TopicId::from_uuid(row.topic_id),
        text: DecisionText::new(&row.text)?,
        status: DecisionStatus::from_str(&row.status)
            .map_err(|source| stored("decision status")(Box::new(source)))?,
        superseded_by: row.superseded_by.map(DecisionId::from_uuid),
        by: row.decided_by,
        session: row
            .session_id
            .map(SessionId::try_from)
            .transpose()
            .map_err(|source| stored("decision session")(Box::new(source)))?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

#[async_trait]
impl DecisionStore for PostgresMemory {
    async fn decisions(&self, topic: TopicId) -> Result<Vec<Decision>, TopicError> {
        sqlx::query_as!(
            DecisionRow,
            "SELECT id, topic_id, text, status, superseded_by, decided_by, session_id,
                    created_at, updated_at
               FROM rh_topic_decisions WHERE topic_id = $1 ORDER BY created_at, id",
            topic.uuid()
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("decision list"))?
        .into_iter()
        .map(decision)
        .collect()
    }

    async fn decision(&self, id: DecisionId) -> Result<Option<Decision>, TopicError> {
        sqlx::query_as!(
            DecisionRow,
            "SELECT id, topic_id, text, status, superseded_by, decided_by, session_id,
                    created_at, updated_at
               FROM rh_topic_decisions WHERE id = $1",
            id.uuid()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("decision read"))?
        .map(decision)
        .transpose()
    }

    async fn keep_decisions(&self, decisions: &[Decision]) -> Result<(), TopicError> {
        let mut tx = self.pool.begin().await.map_err(failed("decision write"))?;
        for decision in decisions {
            let kept = sqlx::query!(
                "INSERT INTO rh_topic_decisions (id, topic_id, text, status, superseded_by,
                                                 decided_by, session_id, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (id) DO UPDATE
                    SET text = EXCLUDED.text, status = EXCLUDED.status,
                        superseded_by = EXCLUDED.superseded_by, updated_at = EXCLUDED.updated_at
                  WHERE rh_topic_decisions.status = 'active'",
                decision.id.uuid(),
                decision.topic.uuid(),
                decision.text.as_str(),
                decision.status.to_string(),
                decision.superseded_by.map(DecisionId::uuid),
                decision.by,
                decision.session.as_ref().map(SessionId::as_str),
                decision.created_at,
                decision.updated_at
            )
            .execute(&mut *tx)
            .await
            .map_err(failed("decision write"))?;
            if kept.rows_affected() == 0 {
                return Err(TopicError::DecisionSuperseded { id: decision.id });
            }
        }
        let topics: BTreeSet<TopicId> = decisions.iter().map(|decision| decision.topic).collect();
        enqueue_syncs(
            &mut tx,
            topics.into_iter().map(SyncTopicPage::topic).collect(),
            self.missing,
        )
        .await?;
        tx.commit().await.map_err(failed("decision write"))
    }
}
