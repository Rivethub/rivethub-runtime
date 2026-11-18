use jiff::Timestamp;
use kernel::topics::HistoryMark;
use kernel::topics::TopicError;
use sqlx::PgPool;
use wire::session_id::SessionId;

use super::rows::failed;
use super::rows::stored;

struct MarkRow {
    session_id: String,
    marked_at: Timestamp,
}

fn mark(row: MarkRow) -> Result<HistoryMark, TopicError> {
    Ok(HistoryMark {
        session: SessionId::try_from(row.session_id)
            .map_err(|error| stored("session_id")(Box::new(error)))?,
        at: row.marked_at,
    })
}

pub(super) async fn keep(pool: &PgPool, kept: &HistoryMark) -> Result<HistoryMark, TopicError> {
    sqlx::query_as!(
        MarkRow,
        "INSERT INTO rh_session_history (session_id, marked_at) VALUES ($1, $2)
         ON CONFLICT (session_id) DO UPDATE SET marked_at = EXCLUDED.marked_at
         RETURNING session_id, marked_at AS \"marked_at: jiff_sqlx::Timestamp\"",
        kept.session.as_str(),
        crate::time::BindTime::bind_time(kept.at) as _
    )
    .fetch_one(pool)
    .await
    .map_err(failed("session history mark"))
    .and_then(mark)
}

pub(super) async fn forget(pool: &PgPool, session: &SessionId) -> Result<(), TopicError> {
    sqlx::query!(
        "DELETE FROM rh_session_history WHERE session_id = $1",
        session.as_str()
    )
    .execute(pool)
    .await
    .map_err(failed("session history unmark"))?;
    Ok(())
}

pub(super) async fn marks(pool: &PgPool) -> Result<Vec<HistoryMark>, TopicError> {
    sqlx::query_as!(
        MarkRow,
        "SELECT session_id, marked_at AS \"marked_at: jiff_sqlx::Timestamp\" FROM rh_session_history
          ORDER BY marked_at DESC, session_id"
    )
    .fetch_all(pool)
    .await
    .map_err(failed("session history"))?
    .into_iter()
    .map(mark)
    .collect()
}
