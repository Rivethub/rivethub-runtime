use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::owner::Owner;
use kernel::review::Mark;
use kernel::review::MarkState;
use kernel::review::ReviewError;
use kernel::review::ReviewKey;
use kernel::review::ReviewMark;
use kernel::review::ReviewStore;

use crate::PostgresMemory;
use crate::error::Error;

const MARKS_SQL: &str =
    "SELECT item_key, state, at FROM rh_review_marks WHERE owner_user_id = $1 ORDER BY item_key";

const MARK_SQL: &str = "INSERT INTO rh_review_marks (owner_user_id, item_key, state, at)
     VALUES ($1, $2, $3, $4)
     ON CONFLICT (owner_user_id, item_key) DO UPDATE SET state = EXCLUDED.state, at = EXCLUDED.at";

#[derive(sqlx::FromRow)]
struct MarkRow {
    item_key: String,
    state: String,
    at: DateTime<Utc>,
}

fn unavailable(error: Error) -> ReviewError {
    ReviewError::Unavailable(Box::new(error))
}

fn mark(row: &MarkRow) -> Result<ReviewMark, Error> {
    let key =
        ReviewKey::from_str(&row.item_key).map_err(|error| Error::stored("review key", error))?;
    let state =
        MarkState::from_str(&row.state).map_err(|error| Error::stored("review state", error))?;
    Ok(ReviewMark {
        key,
        mark: Mark::of(state, row.at),
    })
}

#[async_trait]
impl ReviewStore for PostgresMemory {
    async fn marks(&self, owner: &Owner) -> Result<Vec<ReviewMark>, ReviewError> {
        sqlx::query_as::<_, MarkRow>(MARKS_SQL)
            .bind(owner.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|source| unavailable(Error::operation("review marks", source)))?
            .into_iter()
            .map(|row| mark(&row).map_err(unavailable))
            .collect()
    }

    async fn mark(&self, owner: &Owner, mark: &ReviewMark) -> Result<(), ReviewError> {
        let (state, at) = mark.mark.parts();
        sqlx::query(MARK_SQL)
            .bind(owner.as_str())
            .bind(mark.key.to_string())
            .bind(state.to_string())
            .bind(at)
            .execute(&self.pool)
            .await
            .map_err(|source| unavailable(Error::operation("review mark", source)))?;
        Ok(())
    }
}
