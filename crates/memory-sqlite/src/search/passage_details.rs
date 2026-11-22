use kernel::memory::{
    MemoryEntry, PassageSection, Provenance, SearchOutcome, SummaryDetail, TagWire,
};
use rusqlite::{Connection, params};

use super::{Plan, SqliteError};
use crate::{message::EntryRow, tags, time};

const INTERSECTS: &str = "s.parent_id = p.record_id AND s.kind = 'section'
    AND s.section_char_start < p.char_end AND p.char_start < s.section_char_end";

const OPERATION: &str = "summary passage details";

pub(super) fn hydrate(
    conn: &Connection,
    plan: &Plan,
    outcome: &mut SearchOutcome,
) -> Result<(), SqliteError> {
    for hit in &mut outcome.results {
        let Some(passage) = &hit.passage else {
            continue;
        };
        let id = &passage.id;
        let record = read(conn, id)?;
        hit.title = record
            .title
            .filter(|title| kernel::memory::names_session(title))
            .or_else(|| kernel::memory::summary_title(&record.spoken))
            .or_else(|| kernel::memory::summary_title(&hit.content));
        outcome.summaries.insert(
            id.clone(),
            SummaryDetail {
                kind: record.kind.parse().map_err(|error| SqliteError::Invalid {
                    message: format!("summary kind: {error}"),
                })?,
                earliest_at: Some(time::stored(&record.earliest, "passage earliest time")?),
                latest_at: Some(hit.created_at),
                tags: conversation_tags(conn, &record.conversation)?,
                children: Vec::new(),
                sources: if plan.expand {
                    sources(conn, id, plan)?
                } else {
                    Vec::new()
                },
                sections: sections(conn, id)?,
                provenance: record.settings.as_deref().map(provenance).transpose()?,
            },
        );
    }
    Ok(())
}

struct Record {
    kind: String,
    earliest: String,
    title: Option<String>,
    spoken: String,
    conversation: String,
    settings: Option<String>,
}

fn read(conn: &Connection, id: &str) -> Result<Record, SqliteError> {
    conn.query_row(
        "SELECT s.kind, p.earliest_at, c.title, substr(s.content, 1, 500), c.id,
                CASE WHEN c.session_key LIKE 'import:%' THEN coalesce(c.settings, '{}') END
         FROM ros_summary_passages p JOIN ros_summaries s ON s.id = p.record_id
         JOIN ros_conversations c ON c.id = s.conversation_id WHERE p.id = ?1",
        [id],
        |row| {
            Ok(Record {
                kind: row.get(0)?,
                earliest: row.get(1)?,
                title: row.get(2)?,
                spoken: row.get(3)?,
                conversation: row.get(4)?,
                settings: row.get(5)?,
            })
        },
    )
    .map_err(SqliteError::query(OPERATION))
}

fn provenance(settings: &str) -> Result<Provenance, SqliteError> {
    let settings: wire::JsonObject =
        serde_json::from_str(settings).map_err(|source| SqliteError::StoredJson {
            column: "settings",
            source,
        })?;
    Ok(Provenance::from_settings(&settings))
}

fn conversation_tags(conn: &Connection, id: &str) -> Result<Vec<TagWire>, SqliteError> {
    let columns = tags::TAG_COLUMNS;
    let mut statement = conn
        .prepare(&format!(
            "SELECT {columns} FROM ros_tags WHERE entity_type = 'conversation'
         AND state = 'accepted' AND entity_id = ?1 ORDER BY key, value",
        ))
        .map_err(SqliteError::query(OPERATION))?;
    let rows = statement
        .query_map([id], tags::read_raw)
        .map_err(SqliteError::query(OPERATION))?;
    rows.map(|row| tags::wire_tag(row.map_err(SqliteError::query(OPERATION))?))
        .collect()
}

fn sections(conn: &Connection, id: &str) -> Result<Vec<PassageSection>, SqliteError> {
    let sql = format!(
        "SELECT s.id, substr(s.content, 1, 500), s.source_first, s.source_last, s.range_verified
         FROM ros_summary_passages p JOIN ros_summaries s ON {INTERSECTS}
         WHERE p.id = ?1 ORDER BY s.section_char_start, s.summary_ordinal, s.id"
    );
    let mut statement = conn.prepare(&sql).map_err(SqliteError::query(OPERATION))?;
    let rows = statement
        .query_map([id], |row| {
            let content: String = row.get(1)?;
            Ok(PassageSection {
                id: row.get(0)?,
                heading: content
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .and_then(|line| line.strip_prefix("## "))
                    .unwrap_or("Overview")
                    .trim()
                    .to_owned(),
                source_first: row.get(2)?,
                source_last: row.get(3)?,
                range_verified: row.get(4)?,
            })
        })
        .map_err(SqliteError::query(OPERATION))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query(OPERATION))
}

fn sources(conn: &Connection, id: &str, plan: &Plan) -> Result<Vec<MemoryEntry>, SqliteError> {
    let sql = format!(
        "SELECT m.id, c.session_key, m.agent, m.channel, m.role, substr(m.content, 1, 500),
                  m.tool_name, NULL, NULL, '{{}}', m.created_at, NULL
         FROM (
             SELECT DISTINCT l.message_id FROM ros_summary_passages p
             LEFT JOIN ros_summaries s ON {INTERSECTS}
             JOIN ros_summary_sources l ON l.summary_id = p.record_id
               AND (s.id IS NULL OR l.ordinal BETWEEN s.source_first AND s.source_last)
             WHERE p.id = ?1
         ) picked JOIN ros_messages m ON m.id = picked.message_id
         JOIN ros_conversations c ON c.id = m.conversation_id
         WHERE m.role <> 'tool' AND length(m.content) > 50 AND (?2 IS NULL OR m.agent = ?2)
              AND (?3 IS NULL OR rh_instant(m.created_at) >= ?3)
              AND (?4 IS NULL OR rh_instant(m.created_at) < ?4)
         ORDER BY rh_instant(m.created_at), m.id LIMIT 4"
    );
    let mut statement = conn.prepare(&sql).map_err(SqliteError::query(OPERATION))?;
    let rows = statement
        .query_map(
            params![id, plan.agent, plan.since, plan.before],
            EntryRow::read,
        )
        .map_err(SqliteError::query(OPERATION))?;
    rows.map(|row| {
        let mut entry = MemoryEntry::try_from(row.map_err(SqliteError::query(OPERATION))?)?;
        entry.metadata = None;
        Ok(entry)
    })
    .collect()
}
