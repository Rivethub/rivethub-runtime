use chrono::{DateTime, Utc};
use kernel::owner::Owner;
use kernel::queue::JobId;
use kernel::topics::TopicId;
use kernel::wiki::builder::{
    Coalescable, ExtractWiki, RouteLease, WIKI_PIPELINE_VERSION, WikiStoreError, WikiSummary,
};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::query_error;

#[derive(Debug, sqlx::FromRow)]
struct CandidateRow {
    job: i64,
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
}

pub(super) async fn candidates(
    pool: &PgPool,
    lead: Uuid,
    topic: TopicId,
    limit: u32,
) -> Result<Vec<Coalescable>, WikiStoreError> {
    let queued = crate::graphile::GraphileQueue::schema_present(pool)
        .await
        .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
    if !queued {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, CandidateRow>(
        "SELECT j.id AS job, s.id AS summary_id, s.conversation_id
           FROM graphile_worker._private_jobs j
           JOIN graphile_worker._private_tasks t ON t.id = j.task_id
           JOIN rh_summaries s ON s.id = (j.payload->>'summaryId')::uuid
           JOIN rh_conversations c ON c.id = s.conversation_id
           JOIN rh_session_placements p ON p.session_id = c.session_key
          WHERE t.identifier = 'extract-wiki' AND j.is_available
            AND j.key = 'wiki-ext-' || s.id::text AND p.topic_id = $1 AND s.id <> $2
          ORDER BY j.priority, s.created_at DESC, s.id
          LIMIT $3",
    )
    .bind(topic.uuid())
    .bind(lead)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(query_error)?;
    Ok(rows
        .into_iter()
        .map(|row| Coalescable {
            job: JobId::new(row.job),
            extract: ExtractWiki {
                summary_id: row.summary_id,
                conversation_id: row.conversation_id,
            },
        })
        .collect())
}

const ROUTE_LOCKS: i32 = 0x7769_6b69;

pub(super) async fn lease(
    pool: &PgPool,
    route: &str,
) -> Result<Option<RouteLease>, WikiStoreError> {
    let mut connection = pool.acquire().await.map_err(query_error)?;
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, hashtext($2))")
        .bind(ROUTE_LOCKS)
        .bind(route)
        .fetch_one(&mut *connection)
        .await
        .map_err(query_error)?;
    if !taken {
        return Ok(None);
    }
    connection.close_on_drop();
    Ok(Some(RouteLease::holding(connection)))
}

pub(super) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    summaries: &[&WikiSummary],
    touched: &[String],
    git_sha: Option<&String>,
    now: DateTime<Utc>,
) -> Result<(), WikiStoreError> {
    for summary in summaries {
        sqlx::query(
            "INSERT INTO rh_wiki_extractions (summary_id, status, pipeline_version, topics_touched, extracted_at, git_sha, owner_user_id)
             VALUES ($1, 'done', $2, $3, $4, $5, $6)
             ON CONFLICT (summary_id) DO UPDATE SET status = 'done', pipeline_version = EXCLUDED.pipeline_version,
                topics_touched = EXCLUDED.topics_touched, error = NULL, extracted_at = EXCLUDED.extracted_at, git_sha = EXCLUDED.git_sha",
        )
        .bind(summary.id)
        .bind(WIKI_PIPELINE_VERSION)
        .bind(touched)
        .bind(now)
        .bind(git_sha)
        .bind(summary.owner.as_ref().map(Owner::as_str))
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    }
    Ok(())
}
