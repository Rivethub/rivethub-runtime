use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use rusqlite::Connection;

use crate::Status;
use crate::error::Error;
use crate::upgrade;
use crate::version::SchemaVersion;

const CURRENT: SchemaVersion = SchemaVersion::V31;

const SCHEMA_SQL: &str = include_str!("schema/schema_v7.sql");

const TAIL_INDEXES_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_ros_messages_unembedded ON ros_messages (created_at)
          WHERE embedding IS NULL AND embed_status IS NULL;
CREATE INDEX IF NOT EXISTS idx_ros_conversations_owner ON ros_conversations (owner_user_id)
          WHERE owner_user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_ros_tags_removal_state ON ros_tags (removal_state)
          WHERE removal_state = 'suggested';
";

const CONNECTION_PRAGMAS: &str = "PRAGMA journal_mode = wal;\nPRAGMA foreign_keys = ON;";

pub fn sqlite(path: &Path) -> Result<Status, Error> {
    sqlite_at(path, Utc::now())
}

pub fn sqlite_at(path: &Path, now: DateTime<Utc>) -> Result<Status, Error> {
    if !exists(path)? {
        let mut conn = open_writing(path)?;
        create_fresh(&mut conn)?;
        return Ok(current());
    }
    let found = user_version_read_only(path)?;
    if found == 0 {
        let mut conn = open_writing(path)?;
        create_fresh(&mut conn)?;
        return Ok(current());
    }
    let from = SchemaVersion::try_from(found)?;
    if from == CURRENT {
        return Ok(current());
    }
    copy_original(path, from)?;
    let mut conn = open_writing(path)?;
    bring_through(&mut conn, from, now)?;
    Ok(current())
}

pub fn sqlite_status(path: &Path) -> Result<Status, Error> {
    let required = CURRENT.number();
    if !exists(path)? {
        return Ok(Status {
            current: 0,
            required,
            pending: steps(0),
        });
    }
    match user_version_read_only(path)? {
        0 => Ok(Status {
            current: 0,
            required,
            pending: steps(0),
        }),
        found => {
            let from = SchemaVersion::try_from(found)?;
            Ok(Status {
                current: from.number(),
                required,
                pending: steps(from.number()),
            })
        }
    }
}

fn current() -> Status {
    Status {
        current: CURRENT.number(),
        required: CURRENT.number(),
        pending: 0,
    }
}

fn steps(from: i64) -> usize {
    let mut count = 0usize;
    let mut current = from;
    while current < CURRENT.number() {
        count += 1;
        current += 1;
    }
    count
}

fn open_writing(path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open(path).map_err(|source| Error::SqliteOpen {
        path: path.to_path_buf(),
        source,
    })?;
    conn.execute_batch(CONNECTION_PRAGMAS)
        .map_err(Error::SqlitePragma)?;
    Ok(conn)
}

fn user_version_read_only(path: &Path) -> Result<i64, Error> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|source| Error::SqliteOpen {
            path: path.to_path_buf(),
            source,
        })?;
    user_version(&conn)
}

fn copy_original(path: &Path, from: SchemaVersion) -> Result<(), Error> {
    let backup = backup_path(path, from);
    let mut output = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::BackupExists { path: backup });
        }
        Err(source) => {
            return Err(Error::Backup {
                path: path.to_owned(),
                backup,
                source,
            });
        }
    };
    copy_to_backup(path, &mut output).map_err(|source| Error::Backup {
        path: path.to_path_buf(),
        backup,
        source,
    })?;
    Ok(())
}

fn copy_to_backup(path: &Path, output: &mut std::fs::File) -> Result<(), std::io::Error> {
    let mut input = std::fs::File::open(path)?;
    output.set_permissions(input.metadata()?.permissions())?;
    std::io::copy(&mut input, output)?;
    output.sync_all()
}

fn exists(path: &Path) -> Result<bool, Error> {
    path.try_exists().map_err(|source| Error::Inspect {
        path: path.to_owned(),
        source,
    })
}

fn backup_path(path: &Path, from: SchemaVersion) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".v{}.bak", from.number()));
    PathBuf::from(name)
}

fn create_fresh(conn: &mut Connection) -> Result<(), Error> {
    let tx = begin(conn)?;
    apply_schema(&tx)?;
    tx.execute_batch(include_str!("schema/search_v8.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topics_v11.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_suggestions_v12.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_pages_v13.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_page_sections_v14.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/session_startup_v15.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/startup_delivery_v16.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_decisions_v17.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_page_unbind_v18.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_closed_v19.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/session_history_v19.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/topic_kinds_v20.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/entities_v20.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/conversation_filing_v21.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/suggestion_parent_v21.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/tidy_v22.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/conversation_node_v23.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/session_tombstones_v24.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/tasks_v25.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/trash_lookups_v26.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/job_priority_v27.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/owners_v28.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/session_keeps_v29.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/visibility_v30.sql"))
        .map_err(Error::SqliteCreate)?;
    tx.execute_batch(include_str!("schema/entity_facts_v31.sql"))
        .map_err(Error::SqliteCreate)?;
    apply_tail_indexes(&tx)?;
    stamp_current(&tx)?;
    tx.commit().map_err(Error::SqliteCreate)
}

fn bring_through(
    conn: &mut Connection,
    from: SchemaVersion,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let tx = begin(conn)?;
    apply_schema(&tx)?;
    tx.commit().map_err(Error::SqliteCreate)?;
    upgrade::ladder(conn, from, now)?;
    let tx = begin(conn)?;
    apply_tail_indexes(&tx)?;
    stamp_current(&tx)?;
    tx.commit().map_err(Error::SqliteCreate)
}

fn stamp_current(tx: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    tx.pragma_update(None, "user_version", CURRENT.number())
        .map_err(Error::SqliteCreate)
}

fn user_version(conn: &Connection) -> Result<i64, Error> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(Error::SqlitePragma)
}

fn begin(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>, Error> {
    conn.transaction().map_err(Error::SqliteCreate)
}

fn apply_schema(tx: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(SCHEMA_SQL).map_err(Error::SqliteCreate)
}

fn apply_tail_indexes(tx: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    tx.execute_batch(TAIL_INDEXES_SQL)
        .map_err(Error::SqliteCreate)
}

#[cfg(test)]
mod tests {
    use super::TAIL_INDEXES_SQL;

    #[test]
    fn the_tail_indexes_are_absent_from_the_schema_text() {
        let schema = include_str!("schema/schema_v7.sql");
        for marker in [
            "idx_ros_messages_unembedded",
            "idx_ros_conversations_owner",
            "idx_ros_tags_removal_state",
        ] {
            assert!(
                !schema.contains(marker),
                "{marker} must be applied after the schema text, not inside it"
            );
            assert!(
                TAIL_INDEXES_SQL.contains(marker),
                "{marker} must be created by the tail batch"
            );
        }
    }
}
