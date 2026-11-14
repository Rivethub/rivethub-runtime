use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use kernel::changes::ChangeError;
use kernel::changes::ChangeFeed;
use kernel::changes::ChangeRecord;
use kernel::changes::ChangeWindow;
use kernel::changes::DailyDigest;
use kernel::changes::DigestRequest;
use kernel::changes::DigestStore;
use kernel::changes::TopicRef;
use kernel::owner::Owner;
use kernel::queue::JobQueueError;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use sqlx::PgConnection;
use strum::IntoEnumIterator as _;
use uuid::Uuid;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::PostgresMemory;
use crate::error::Error;
use crate::facts::ClaimRow;

const IN_WINDOW: &str = "($1::timestamptz IS NULL OR AT >= $1) AND AT < $2";

const CLAIMS_SQL: &str = "SELECT (SELECT g.id FROM rh_entity_facts g
                   WHERE g.topic_id = f.topic_id AND g.key = f.key
                   ORDER BY g.created_at, g.id LIMIT 1) AS fact,
            f.key, c.value, c.actor, c.pinned, c.session_id, c.summary_id, c.evidence,
            c.at, c.recorded_at, c.owner_user_id, c.settled, t.id AS entity, t.name AS title
       FROM rh_entity_fact_claims c
       JOIN rh_entity_facts f ON f.id = c.fact_id
       JOIN rh_topics t ON t.id = f.topic_id
      WHERE c.at < $2
        AND f.topic_id IN (SELECT w.topic_id FROM rh_entity_facts w
                             JOIN rh_entity_fact_claims n ON n.fact_id = w.id
                            WHERE WINDOW)
      ORDER BY c.at, c.recorded_at, c.id";

const DECISIONS_SQL: &str = "SELECT t.id, t.name, t.kind, d.text, d.session_id, d.created_at AS at
       FROM rh_topic_decisions d JOIN rh_topics t ON t.id = d.topic_id
      WHERE WINDOW";

const CREATED_SQL: &str = "SELECT t.id, t.name, t.kind, t.created_at AS at FROM rh_topics t
      WHERE t.kind IN ('work', 'entity') AND WINDOW";

const PLACED_SQL: &str = "SELECT t.id, t.name, t.kind, p.session_id, p.placed_by, p.placed_at AS at
       FROM rh_session_placements p JOIN rh_topics t ON t.id = p.topic_id
      WHERE WINDOW";

const TAGGED_SQL: &str = "SELECT k.session_key, k.key, k.value, k.at FROM (
         SELECT CASE WHEN cs.channel = $4 AND cs.session_key ~* $5 THEN $4 || ':' || cs.session_key
                     ELSE cs.session_key END AS session_key, g.key, g.value,
                coalesce(g.decided_at, g.updated_at) AS at
           FROM rh_tags g
           LEFT JOIN rh_conversations c ON g.entity_type = 'conversation' AND c.id = g.entity_id
           LEFT JOIN rh_summaries s ON g.entity_type = 'summary' AND s.id = g.entity_id
           LEFT JOIN rh_conversations sc ON sc.id = s.conversation_id
           CROSS JOIN LATERAL (SELECT coalesce(c.channel, sc.channel) AS channel,
                                      coalesce(c.session_key, sc.session_key) AS session_key) cs
          WHERE g.state = 'accepted'
     ) k
      WHERE split_part(k.session_key, ':', 1) = ANY($3)
        AND substr(k.session_key, strpos(k.session_key, ':') + 1) <> ''
        AND WINDOW";

const MERGED_SQL: &str = "SELECT t.id, t.name, t.kind, r.merged_from, r.created_at AS at
       FROM rh_topic_redirects r JOIN rh_topics t ON t.id = r.topic_id
      WHERE r.merged_from IS NOT NULL AND WINDOW";

#[derive(sqlx::FromRow)]
struct ClaimChange {
    #[sqlx(flatten)]
    claim: ClaimRow,
    entity: Uuid,
    title: String,
}

#[derive(sqlx::FromRow)]
struct DecisionRow {
    id: Uuid,
    name: String,
    kind: String,
    text: String,
    session_id: Option<String>,
    at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct CreatedRow {
    id: Uuid,
    name: String,
    kind: String,
    at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PlacedRow {
    id: Uuid,
    name: String,
    kind: String,
    session_id: String,
    placed_by: String,
    at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct TaggedRow {
    session_key: String,
    key: String,
    value: String,
    at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct MergedRow {
    id: Uuid,
    name: String,
    kind: String,
    merged_from: String,
    at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct DigestRow {
    summary: String,
    changes: i32,
    written_at: DateTime<Utc>,
}

fn failed(operation: &'static str) -> impl FnOnce(sqlx::Error) -> ChangeError {
    move |source| ChangeError::Unavailable(Box::new(Error::operation(operation, source)))
}

fn stored(
    field: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ChangeError {
    ChangeError::Unavailable(Box::new(Error::StoredValue {
        field,
        source: Box::new(source),
    }))
}

fn windowed(sql: &str, column: &str) -> String {
    sql.replace("WINDOW", &IN_WINDOW.replace("AT", column))
}

fn topic(id: Uuid, title: String, kind: &str) -> Result<TopicRef, ChangeError> {
    Ok(TopicRef {
        id: TopicId::from_uuid(id),
        title,
        kind: TopicKind::from_str(kind).map_err(|error| stored("topic kind", error))?,
    })
}

fn session(key: &str) -> Result<SessionId, ChangeError> {
    SessionId::from_str(key).map_err(|error| stored("session id", error))
}

async fn fetch<T>(
    conn: &mut PgConnection,
    sql: &str,
    window: &ChangeWindow,
) -> Result<Vec<T>, ChangeError>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin,
{
    let harnesses: Vec<String> = HarnessId::iter()
        .map(|harness| harness.to_string())
        .collect();
    let query = sqlx::query_as::<_, T>(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(window.from)
        .bind(window.to);
    let query = if sql.contains("$3") {
        query
            .bind(harnesses)
            .bind(HarnessId::ClaudeCode.to_string())
            .bind(crate::session_catalog::BARE_UUID)
    } else {
        query
    };
    query
        .fetch_all(&mut *conn)
        .await
        .map_err(failed("change feed"))
}

async fn records(
    conn: &mut PgConnection,
    window: &ChangeWindow,
) -> Result<Vec<ChangeRecord>, ChangeError> {
    let mut found = Vec::new();
    for row in fetch::<ClaimChange>(conn, &windowed(CLAIMS_SQL, "n.at"), window).await? {
        found.push(ChangeRecord::Claim {
            entity: TopicId::from_uuid(row.entity),
            title: row.title,
            claim: crate::facts::claim(&row.claim).map_err(|error| stored("fact claim", error))?,
        });
    }
    for row in fetch::<DecisionRow>(conn, &windowed(DECISIONS_SQL, "d.created_at"), window).await? {
        found.push(ChangeRecord::Decision {
            topic: topic(row.id, row.name, &row.kind)?,
            text: row.text,
            session: row.session_id.as_deref().map(session).transpose()?,
            at: row.at,
        });
    }
    for row in fetch::<CreatedRow>(conn, &windowed(CREATED_SQL, "t.created_at"), window).await? {
        found.push(ChangeRecord::Created {
            topic: topic(row.id, row.name, &row.kind)?,
            at: row.at,
        });
    }
    for row in fetch::<PlacedRow>(conn, &windowed(PLACED_SQL, "p.placed_at"), window).await? {
        found.push(ChangeRecord::Placed {
            topic: topic(row.id, row.name, &row.kind)?,
            session: session(&row.session_id)?,
            by: PlacementSource::from_str(&row.placed_by)
                .map_err(|error| stored("placed_by", error))?,
            at: row.at,
        });
    }
    for row in fetch::<TaggedRow>(conn, &windowed(TAGGED_SQL, "k.at"), window).await? {
        found.push(ChangeRecord::Tagged {
            session: session(&row.session_key)?,
            key: row.key,
            value: row.value,
            at: row.at,
        });
    }
    for row in fetch::<MergedRow>(conn, &windowed(MERGED_SQL, "r.created_at"), window).await? {
        found.push(ChangeRecord::Merged {
            into: topic(row.id, row.name, &row.kind)?,
            from: row.merged_from,
            at: row.at,
        });
    }
    Ok(found)
}

#[async_trait]
impl ChangeFeed for PostgresMemory {
    async fn records(&self, window: &ChangeWindow) -> Result<Vec<ChangeRecord>, ChangeError> {
        let mut conn = self.pool.acquire().await.map_err(failed("change feed"))?;
        records(&mut conn, window).await
    }
}

#[async_trait]
impl DigestStore for PostgresMemory {
    async fn digest(
        &self,
        day: NaiveDate,
        owner: &Owner,
    ) -> Result<Option<DailyDigest>, ChangeError> {
        let found = sqlx::query_as::<_, DigestRow>(
            "SELECT summary, changes, written_at FROM rh_daily_digests
              WHERE owner_user_id = $1 AND day = $2",
        )
        .bind(owner.as_str())
        .bind(day)
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("daily digest"))?;
        found
            .map(|row| {
                Ok(DailyDigest {
                    day,
                    owner: owner.clone(),
                    summary: row.summary,
                    changes: u32::try_from(row.changes)
                        .map_err(|error| stored("digest changes", error))?,
                    written_at: row.written_at,
                })
            })
            .transpose()
    }

    async fn keep_digest(&self, digest: &DailyDigest) -> Result<(), ChangeError> {
        sqlx::query(
            "INSERT INTO rh_daily_digests (owner_user_id, day, summary, changes, written_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (owner_user_id, day) DO UPDATE SET summary = EXCLUDED.summary,
                    changes = EXCLUDED.changes, written_at = EXCLUDED.written_at",
        )
        .bind(digest.owner.as_str())
        .bind(digest.day)
        .bind(&digest.summary)
        .bind(i32::try_from(digest.changes).map_err(|error| stored("digest changes", error))?)
        .bind(digest.written_at)
        .execute(&self.pool)
        .await
        .map_err(failed("daily digest"))?;
        Ok(())
    }

    async fn request_digest(&self, request: &DigestRequest) -> Result<(), ChangeError> {
        let job = request.job()?;
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(failed("daily digest job"))?;
        match crate::jobs::enqueue_job(&mut conn, &job).await {
            Ok(_) => {}
            Err(JobQueueError::Absent) => return Ok(()),
            Err(source) => return Err(ChangeError::Unavailable(Box::new(source))),
        }
        Ok(())
    }
}
