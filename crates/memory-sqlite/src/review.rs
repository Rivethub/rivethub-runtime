use std::str::FromStr as _;

use async_trait::async_trait;
use kernel::owner::Owner;
use kernel::review::Mark;
use kernel::review::MarkState;
use kernel::review::ReviewError;
use kernel::review::ReviewKey;
use kernel::review::ReviewMark;
use kernel::review::ReviewStore;
use rusqlite::params;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const MARKS_SQL: &str =
    "SELECT item_key, state, at FROM rh_review_marks WHERE owner_user_id = ?1 ORDER BY item_key";

const MARK_SQL: &str = "INSERT INTO rh_review_marks (owner_user_id, item_key, state, at)
     VALUES (?1, ?2, ?3, ?4)
     ON CONFLICT (owner_user_id, item_key) DO UPDATE SET state = excluded.state, at = excluded.at";

fn unavailable(error: SqliteError) -> ReviewError {
    ReviewError::Unavailable(Box::new(error))
}

fn invalid(message: String) -> SqliteError {
    SqliteError::Invalid { message }
}

fn mark(key: &str, state: &str, at: &str) -> Result<ReviewMark, SqliteError> {
    let key = ReviewKey::from_str(key).map_err(|error| invalid(error.to_string()))?;
    let state = MarkState::from_str(state)
        .map_err(|error| invalid(format!("stored review state {state:?}: {error}")))?;
    Ok(ReviewMark {
        key,
        mark: Mark::of(state, time::stored(at, "at")?),
    })
}

#[async_trait]
impl ReviewStore for SqliteStore {
    async fn marks(&self, owner: &Owner) -> Result<Vec<ReviewMark>, ReviewError> {
        let owner = owner.as_str().to_owned();
        self.with_conn(move |conn| {
            let rows = conn
                .prepare(MARKS_SQL)
                .and_then(|mut statement| {
                    statement
                        .query_map(params![owner], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .map_err(SqliteError::query("review marks"))?;
            rows.iter()
                .map(|(key, state, at)| mark(key, state, at))
                .collect()
        })
        .await
        .map_err(unavailable)
    }

    async fn mark(&self, owner: &Owner, mark: &ReviewMark) -> Result<(), ReviewError> {
        let owner = owner.as_str().to_owned();
        let key = mark.key.to_string();
        let (state, at) = mark.mark.parts();
        self.with_conn(move |conn| {
            conn.execute(
                MARK_SQL,
                params![owner, key, state.to_string(), time::stamp(at)],
            )
            .map_err(SqliteError::query("review mark"))?;
            Ok(())
        })
        .await
        .map_err(unavailable)
    }
}
