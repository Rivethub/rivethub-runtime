use jiff::Timestamp;
use kernel::filing::Filable;
use kernel::memory::summary_opening::{OpeningConsumer, opening_text};
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{MemoryBrowseOrder, needs_title};
use kernel::queue::Job;
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;
use wire::{JsonObject, message::Role};

use crate::capture::CaptureWriter;
use crate::graphile::{GraphileQueue, enqueue_on};
use crate::{Error, PostgresMemory};

#[derive(sqlx::FromRow)]
struct Turn {
    role: Json<Role>,
    metadata: Option<Json<JsonObject>>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    content: String,
}

impl CaptureWriter<'_> {
    pub(crate) async fn refresh_opening(
        &mut self,
        id: Uuid,
        language_available: bool,
    ) -> Result<(), Error> {
        let Some(content) = projection(&mut self.tx, id).await? else {
            return Ok(());
        };
        let old = crate::title::load(&mut self.tx, id).await?;
        if old.as_ref().is_some_and(|old| old.content == content) {
            return Ok(());
        }
        let (head, supplied) = crate::title::head(&mut self.tx, id).await?;
        let filing = Filable::of(&head) != Filable::Never;
        let title = filing && !supplied && needs_title(head.title.as_deref());
        sqlx::query(
            "INSERT INTO rh_summary_openings (conversation_id, content, filing_armed, title_armed)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (conversation_id) DO UPDATE SET content = excluded.content",
        )
        .bind(id)
        .bind(&content)
        .bind(filing)
        .bind(title)
        .execute(&mut *self.tx)
        .await
        .map_err(failure)?;
        if !supplied {
            crate::title::seed(&mut self.tx, &head, old.as_ref(), &content).await?;
        }
        if old.is_none() && self.queue_present && language_available {
            for (arm, consumer) in [
                (filing, OpeningConsumer::Filing),
                (title, OpeningConsumer::Title),
            ] {
                if arm {
                    enqueue_on(&mut *self.tx, &consumer.job(id)?).await?;
                }
            }
        }
        Ok(())
    }
}

async fn projection(conn: &mut PgConnection, id: Uuid) -> Result<Option<String>, Error> {
    let turns: Vec<Turn> = sqlx::query_as(
        "SELECT to_json(role) AS role, created_at,
                CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata END AS metadata,
                CASE WHEN role = 'user' THEN content ELSE '' END AS content
           FROM rh_messages WHERE conversation_id = $1 ORDER BY created_at, id",
    )
    .bind(id)
    .fetch_all(conn)
    .await
    .map_err(failure)?;
    let ordered = order_messages(turns, MemoryBrowseOrder::Asc, |turn| {
        MessagePosition::new(
            turn.created_at,
            turn.role.0,
            turn.metadata.as_ref().map(|json| &json.0),
        )
    });
    Ok(opening_text(
        ordered
            .iter()
            .map(|turn| (turn.role.0, turn.content.as_str())),
    ))
}

pub(crate) async fn seed_historical(conn: &mut PgConnection, id: Uuid) -> Result<bool, Error> {
    let Some(content) = projection(conn, id).await? else {
        return Ok(false);
    };
    sqlx::query(
        "INSERT INTO rh_summary_openings (conversation_id, content, historical_pending)
         VALUES ($1, $2, true) ON CONFLICT (conversation_id) DO NOTHING",
    )
    .bind(id)
    .bind(content)
    .execute(conn)
    .await
    .map_err(failure)?;
    Ok(true)
}

impl PostgresMemory {
    pub(crate) async fn undelivered_openings(&self) -> Result<Vec<Job>, Error> {
        if !GraphileQueue::schema_present(&self.pool).await? {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, bool)> = sqlx::query_as(
            "SELECT o.conversation_id, work.filing
               FROM rh_summary_openings o
              CROSS JOIN LATERAL (VALUES (true, o.filing_armed, 'file-'),
                                         (false, o.title_armed, 'title-')) AS work(filing, armed, prefix)
              WHERE work.armed AND NOT EXISTS (
                  SELECT 1 FROM graphile_worker._private_jobs j
                   WHERE j.key = work.prefix || o.conversation_id::text
                     AND (j.attempts < j.max_attempts OR j.locked_at >= now() - interval '4 hours')
              )
              ORDER BY o.conversation_id, work.filing LIMIT 256",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failure)?;
        rows.into_iter()
            .map(|(id, filing)| {
                let consumer = if filing {
                    OpeningConsumer::Filing
                } else {
                    OpeningConsumer::Title
                };
                consumer.job(id).map_err(Error::from)
            })
            .collect()
    }

    pub(crate) async fn opening_filing(&self, id: Uuid) -> Result<Option<bool>, Error> {
        sqlx::query_scalar(
            "SELECT filing_armed FROM rh_summary_openings WHERE conversation_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(failure)
    }

    pub(crate) async fn finish_opening_filing(&self, id: Uuid) -> Result<(), Error> {
        sqlx::query("UPDATE rh_summary_openings SET filing_armed = false WHERE conversation_id = $1 AND filing_armed")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(failure)?;
        Ok(())
    }
}

fn failure(source: sqlx::Error) -> Error {
    Error::operation("summary opening", source)
}
