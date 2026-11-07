use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;
use rusqlite::params;

use crate::error::SqliteError;
use crate::time;

pub(crate) fn trashed_at(
    conn: &Connection,
    keys: &str,
) -> Result<BTreeMap<String, DateTime<Utc>>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT session_key, trashed_at FROM rh_session_trash
              WHERE session_key IN (SELECT value FROM json_each(?1))",
        )
        .map_err(SqliteError::query("session trash"))?;
    let rows = statement
        .query_map([keys], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(SqliteError::query("session trash"))?;
    let mut trashed = BTreeMap::new();
    for row in rows {
        let (session_key, stored) = row.map_err(SqliteError::query("session trash"))?;
        trashed.insert(
            session_key,
            time::stored(&stored, "rh_session_trash.trashed_at")?,
        );
    }
    Ok(trashed)
}

pub(crate) fn due_trash(
    conn: &Connection,
    older_than: DateTime<Utc>,
    limit: u32,
) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT session_key FROM rh_session_trash
              WHERE julianday(trashed_at) <= julianday(?1)
              ORDER BY julianday(trashed_at), session_key
              LIMIT ?2",
        )
        .map_err(SqliteError::query("trash sweep"))?;
    let rows = statement
        .query_map(params![time::stamp(older_than), limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(SqliteError::query("trash sweep"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("trash sweep"))
}

pub(crate) fn kept_at(
    conn: &Connection,
    keys: &str,
) -> Result<BTreeMap<String, DateTime<Utc>>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT session_key, kept_at FROM rh_session_keeps
              WHERE session_key IN (SELECT value FROM json_each(?1))",
        )
        .map_err(SqliteError::query("session keeps"))?;
    let rows = statement
        .query_map([keys], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(SqliteError::query("session keeps"))?;
    let mut kept = BTreeMap::new();
    for row in rows {
        let (session_key, stored) = row.map_err(SqliteError::query("session keeps"))?;
        kept.insert(
            session_key,
            time::stored(&stored, "rh_session_keeps.kept_at")?,
        );
    }
    Ok(kept)
}
