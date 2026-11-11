use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use rusqlite::params;

use crate::error::Error;
use crate::version::SchemaVersion;

pub(crate) fn ladder(
    conn: &mut Connection,
    from: SchemaVersion,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut current = from;
    while let Some(next) = current.next() {
        step(conn, next, now)?;
        current = next;
    }
    Ok(())
}

fn step(conn: &mut Connection, to: SchemaVersion, now: DateTime<Utc>) -> Result<(), Error> {
    match to {
        SchemaVersion::V1 | SchemaVersion::V2 | SchemaVersion::V4 | SchemaVersion::V5 => Ok(()),
        SchemaVersion::V3 => in_transaction(conn, to, |tx| {
            add_message_embedding_columns(tx)?;
            move_embed_queue_into_jobs(tx, now)
        }),
        SchemaVersion::V6 => in_transaction(conn, to, add_owner_columns),
        SchemaVersion::V7 => in_transaction(conn, to, add_tag_removal_columns),
        SchemaVersion::V8 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/search_v8.sql"))
        }),
        SchemaVersion::V9 => rewrite_legacy_summary_kinds(conn),
        SchemaVersion::V10 => in_transaction(conn, to, crate::ids::canonicalize),
        SchemaVersion::V11 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/topics_v11.sql"))
        }),
        SchemaVersion::V12 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/topic_suggestions_v12.sql"))
        }),
        SchemaVersion::V13 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/topic_pages_v13.sql"))
        }),
        SchemaVersion::V14 => in_transaction(conn, to, add_topic_page_sections),
        SchemaVersion::V15 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/session_startup_v15.sql"))
        }),
        SchemaVersion::V16 => in_transaction(conn, to, add_startup_delivery),
        SchemaVersion::V17 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/topic_decisions_v17.sql"))
        }),
        SchemaVersion::V18 => in_transaction(conn, to, unbind_topic_pages_on_delete),
        SchemaVersion::V19 => in_transaction(conn, to, add_done_state),
        SchemaVersion::V20 => in_transaction(conn, to, add_topic_kinds),
        SchemaVersion::V21 => in_transaction(conn, to, add_conversation_filing),
        SchemaVersion::V22 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/tidy_v22.sql"))
        }),
        SchemaVersion::V23 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/conversation_node_v23.sql"))
        }),
        SchemaVersion::V24 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/session_tombstones_v24.sql"))
        }),
        SchemaVersion::V25 => in_transaction(conn, to, archive_tasks),
        SchemaVersion::V26 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/trash_lookups_v26.sql"))
        }),
        SchemaVersion::V27 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/job_priority_v27.sql"))
        }),
        SchemaVersion::V28 => in_transaction(conn, to, add_owners),
        SchemaVersion::V29 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/session_keeps_v29.sql"))
        }),
        SchemaVersion::V30 => in_transaction(conn, to, add_visibility),
        SchemaVersion::V31 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/session_trash_v31.sql"))
        }),
        SchemaVersion::V32 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/entity_facts_v32.sql"))
        }),
        SchemaVersion::V33 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/review_marks_v33.sql"))
        }),
        SchemaVersion::V34 => in_transaction(conn, to, add_daily_digest),
        SchemaVersion::V35 => in_transaction(conn, to, |tx| {
            tx.execute_batch(include_str!("schema/model_calls_v35.sql"))
        }),
    }
}

fn add_daily_digest(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(include_str!("schema/daily_digest_v34.sql"))?;
    if column_present(tx, "rh_topic_redirects", "merged_from")? {
        return Ok(());
    }
    tx.execute_batch(include_str!("schema/merged_from_v34.sql"))
}

fn add_visibility(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if column_present(tx, "rh_topics", "visibility")? {
        return Ok(());
    }
    tx.execute_batch(include_str!("schema/visibility_v30.sql"))
}

fn add_owners(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if !column_present(tx, "rh_topics", "owner_user_id")? {
        tx.execute_batch(include_str!("schema/owners_v28.sql"))?;
    }
    tx.execute_batch(include_str!("schema/owners_backfill_v28.sql"))
}

const ARCHIVE_TASKS: &str = "
CREATE TABLE rh_tasks_archive AS SELECT * FROM rh_tasks;
DROP TABLE rh_tasks;
UPDATE rh_tasks_archive
   SET status = 'killed',
       completed_at = COALESCE(completed_at, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
       error = COALESCE(error, 'archived with the tasks refactor')
 WHERE status IN ('queued', 'running', 'awaiting-input');
";

fn archive_tasks(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let legacy: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_table_info('rh_tasks') WHERE name = 'executor')",
        [],
        |row| row.get(0),
    )?;
    if legacy {
        tx.execute_batch(ARCHIVE_TASKS)?;
    }
    tx.execute_batch(include_str!("schema/tasks_v25.sql"))
}

const LEGACY_KIND_REWRITE: &str = "
UPDATE ros_summaries
SET kind = CASE
    WHEN depth = 0 THEN 'leaf'
    WHEN depth = 1 THEN 'branch'
    WHEN depth >= 2 THEN 'root'
END
WHERE kind NOT IN ('leaf', 'branch', 'root')
  AND depth >= 0;
";

fn rewrite_legacy_summary_kinds(conn: &mut Connection) -> Result<(), Error> {
    let failed = |source| Error::SqliteUpgrade {
        version: SchemaVersion::V9.number(),
        source,
    };
    let tx = conn.transaction().map_err(failed)?;
    tx.execute_batch(LEGACY_KIND_REWRITE).map_err(failed)?;
    let left: i64 = tx
        .query_row(
            "SELECT count(*) FROM ros_summaries WHERE kind NOT IN ('leaf', 'branch', 'root')",
            [],
            |row| row.get(0),
        )
        .map_err(failed)?;
    if left > 0 {
        return Err(Error::UnmappedSummaryKind);
    }
    tx.commit().map_err(failed)?;
    Ok(())
}

fn in_transaction(
    conn: &mut Connection,
    to: SchemaVersion,
    run: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
) -> Result<(), Error> {
    let failed = |source: rusqlite::Error| Error::SqliteUpgrade {
        version: to.number(),
        source,
    };
    let tx = conn.transaction().map_err(failed)?;
    run(&tx).map_err(failed)?;
    tx.commit().map_err(failed)
}

fn add_message_embedding_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if column_present(tx, "ros_messages", "embedding")? {
        return Ok(());
    }
    tx.execute_batch(
        "ALTER TABLE ros_messages ADD COLUMN embedding BLOB;
         ALTER TABLE ros_messages ADD COLUMN embed_error TEXT;
         ALTER TABLE ros_messages ADD COLUMN embed_failures INTEGER NOT NULL DEFAULT 0;",
    )
}

fn add_owner_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if !column_present(tx, "ros_conversations", "owner_user_id")? {
        tx.execute(
            "ALTER TABLE ros_conversations ADD COLUMN owner_user_id TEXT",
            [],
        )?;
    }
    if !column_present(tx, "ros_messages", "owner_user_id")? {
        tx.execute("ALTER TABLE ros_messages ADD COLUMN owner_user_id TEXT", [])?;
    }
    Ok(())
}

fn add_topic_page_sections(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if column_present(tx, "rh_topic_pages", "sections")? {
        return Ok(());
    }
    tx.execute_batch(include_str!("schema/topic_page_sections_v14.sql"))
}

fn add_startup_delivery(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if column_present(tx, "rh_session_startup", "delivered_via")? {
        return Ok(());
    }
    tx.execute_batch(include_str!("schema/startup_delivery_v16.sql"))
}

fn add_done_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if !column_present(tx, "rh_topics", "closed_at")? {
        tx.execute_batch(include_str!("schema/topic_closed_v19.sql"))?;
    }
    tx.execute_batch(include_str!("schema/session_history_v19.sql"))
}

fn add_topic_kinds(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if !column_present(tx, "rh_topics", "kind")? {
        tx.execute_batch(include_str!("schema/topic_kinds_v20.sql"))?;
    }
    tx.execute_batch(include_str!("schema/entities_v20.sql"))
}

fn add_conversation_filing(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(include_str!("schema/conversation_filing_v21.sql"))?;
    if column_present(tx, "rh_session_suggestions", "parent_id")? {
        return Ok(());
    }
    tx.execute_batch(include_str!("schema/suggestion_parent_v21.sql"))
}

const UNBIND_KEEPING_OWNER: &str = "
CREATE TABLE rh_topic_pages_v18 (
    page_slug TEXT NOT NULL PRIMARY KEY REFERENCES ros_wiki_topics (slug) ON DELETE CASCADE,
    topic_id TEXT UNIQUE REFERENCES rh_topics (id) ON DELETE SET NULL,
    sections TEXT NOT NULL DEFAULT '[]',
    owner_user_id TEXT
);
INSERT INTO rh_topic_pages_v18 (page_slug, topic_id, sections, owner_user_id)
    SELECT page_slug, topic_id, sections, owner_user_id FROM rh_topic_pages;
DROP TABLE rh_topic_pages;
CREATE TABLE rh_topic_pages (
    page_slug TEXT NOT NULL PRIMARY KEY REFERENCES ros_wiki_topics (slug) ON DELETE CASCADE,
    topic_id TEXT UNIQUE REFERENCES rh_topics (id) ON DELETE SET NULL,
    sections TEXT NOT NULL DEFAULT '[]',
    owner_user_id TEXT
);
INSERT INTO rh_topic_pages (page_slug, topic_id, sections, owner_user_id)
    SELECT page_slug, topic_id, sections, owner_user_id FROM rh_topic_pages_v18;
DROP TABLE rh_topic_pages_v18;
";

fn unbind_topic_pages_on_delete(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let keyed_by_page = tx
        .query_row(
            "SELECT pk FROM pragma_table_info('rh_topic_pages') WHERE name = 'page_slug'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        == Some(1);
    if keyed_by_page {
        return Ok(());
    }
    if column_present(tx, "rh_topic_pages", "owner_user_id")? {
        tx.execute_batch(UNBIND_KEEPING_OWNER)
    } else {
        tx.execute_batch(include_str!("schema/topic_page_unbind_v18.sql"))
    }
}

fn add_tag_removal_columns(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    if column_present(tx, "ros_tags", "removal_state")? {
        return Ok(());
    }
    tx.execute_batch(
        "ALTER TABLE ros_tags ADD COLUMN removal_state TEXT CHECK (removal_state IS NULL OR removal_state IN ('suggested', 'rejected'));
         ALTER TABLE ros_tags ADD COLUMN removal_reason TEXT NOT NULL DEFAULT '';",
    )
}

fn move_embed_queue_into_jobs(tx: &Transaction<'_>, now: DateTime<Utc>) -> rusqlite::Result<()> {
    let stamp = timestamp(now);
    tx.execute(
        r"INSERT INTO ros_jobs (id, task, job_key, payload, run_at, attempts, max_attempts, state, last_error, locked_at, created_at, updated_at)
           SELECT id, 'embed-target', 'embed-ros_messages-' || message_id,
                  json_object('targetTable', 'ros_messages', 'targetId', message_id),
                  enqueued_at, 0, 5, 'queued', NULL, NULL, ?1, ?1
           FROM ros_embed_queue
           ORDER BY rowid",
        params![stamp],
    )?;
    tx.execute("DELETE FROM ros_embed_queue", [])?;
    Ok(())
}

fn column_present(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|found| found.is_some())
}

#[must_use]
fn timestamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::timestamp;
    use chrono::TimeZone;
    use chrono::Utc;

    #[test]
    fn timestamps_render_as_millisecond_instant_text() {
        let now = Utc.with_ymd_and_hms(2026, 10, 10, 12, 0, 0).unwrap();
        assert_eq!(timestamp(now), "2026-10-10T12:00:00.000Z");
    }
}
