use std::collections::BTreeSet;
use std::path::Path;

use kernel::memory::summary_conversion::{ConversionRehearsal, REHEARSAL_LIMIT};
use kernel::memory::summary_reference::replacement_target;
use rusqlite::{Connection, OpenFlags, params};
use uuid::Uuid;

use crate::{SqliteError, SqliteStore, summary_replace::publish::old_coverage};

fn error(source: rusqlite::Error) -> SqliteError {
    SqliteError::query("rehearse summary references")(source)
}

const REFERENCES: &[(&str, &str, &str, &str)] = &[
    ("ros_summary_sources", "summary_id", "1", "false"),
    ("ros_wiki_citations", "summary_id", "r.topic_slug", "true"),
    (
        "ros_wiki_provenance",
        "source_id",
        "r.topic_slug",
        "r.source_kind = 'summary'",
    ),
    ("ros_wiki_extractions", "summary_id", "1", "true"),
    ("rh_wiki_deferred", "summary_id", "1", "true"),
    ("rh_entity_fact_claims", "summary_id", "r.fact_id", "true"),
    (
        "ros_tags",
        "entity_id",
        "r.key, r.value",
        "r.entity_type = 'summary'",
    ),
    ("ros_summary_deliveries", "anchor_id", "1", "false"),
    ("rh_summary_runs", "expected_root", "1", "false"),
    (
        "rh_wiki_entity_candidates",
        "summary_id",
        "r.key, CASE WHEN r.source = r.summary_id THEN ':root'
                     WHEN r.source = 'summary:' || r.summary_id THEN 'summary::root'
                     ELSE r.source END",
        "true",
    ),
];

impl SqliteStore {
    pub fn rehearse_conversion(
        path: &Path,
        ids: &[Uuid],
    ) -> Result<ConversionRehearsal, SqliteError> {
        if ids.is_empty() || ids.len() > REHEARSAL_LIMIT {
            return Err(SqliteError::Invalid {
                message: "the rehearsal requires 1 to 20 conversation IDs".into(),
            });
        }
        let mut conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(error)?;
        let tx = conn.transaction().map_err(error)?;
        let mut result = ConversionRehearsal::empty();
        let unique: BTreeSet<_> = ids.iter().copied().collect();
        for id in unique {
            rehearse(&tx, id, &mut result)?;
        }
        tx.commit().map_err(error)?;
        Ok(result)
    }
}

fn rehearse(
    conn: &Connection,
    id: Uuid,
    result: &mut ConversionRehearsal,
) -> Result<(), SqliteError> {
    let old = select_ids(
        conn,
        "SELECT id FROM ros_summaries WHERE conversation_id = ?1 AND kind IN ('leaf','branch','root')",
        id,
    )?;
    if old.is_empty() {
        return Ok(());
    }
    let coverage = old_coverage(conn, id)?;
    let renderable = select_ids(
        conn,
        "SELECT id FROM ros_messages WHERE conversation_id = ?1
           AND (content <> '' OR coalesce(tool_name <> '', false))",
        id,
    )?;
    let mapped: Vec<_> = old
        .iter()
        .filter(|id| {
            coverage.get(id).is_some_and(|members| {
                replacement_target(members, [(Uuid::nil(), &renderable)]).is_some()
            })
        })
        .collect();
    let old = encode_ids(&old)?;
    let mapped = encode_ids(&mapped)?;
    result.sampled_conversations += 1;
    for &(table, link, grouping, condition) in REFERENCES {
        let filter = match table {
            "ros_wiki_provenance" => "r.source_kind = 'summary'",
            "ros_tags" => "r.entity_type = 'summary'",
            _ => "true",
        };
        let (total, remapped) = conn.query_row(&format!(
            "WITH affected AS (
                SELECT r.* FROM {table} r WHERE r.{link} IN (SELECT value FROM json_each(?1)) AND {filter}
             )
             SELECT (SELECT count(*) FROM affected),
                    (SELECT count(*) FROM (SELECT 1 FROM affected r
                       WHERE r.{link} IN (SELECT value FROM json_each(?2)) AND ({condition})
                       GROUP BY {grouping}))"
        ), params![old, mapped], |row| Ok((row.get(0)?, row.get(1)?))).map_err(error)?;
        result.add(table, total, remapped);
    }
    let queued = conn
        .query_row(
            "SELECT count(*) FROM ros_jobs WHERE state <> 'running' AND json_valid(payload) AND (
           json_extract(payload, '$.summaryId') IN (SELECT value FROM json_each(?1)) OR (
              json_extract(payload, '$.targetTable') = 'ros_summaries'
              AND json_extract(payload, '$.targetId') IN (SELECT value FROM json_each(?1))))",
            [old],
            |row| row.get(0),
        )
        .map_err(error)?;
    result.add("ros_jobs", queued, 0);
    Ok(())
}

fn select_ids(conn: &Connection, sql: &str, id: Uuid) -> Result<BTreeSet<Uuid>, SqliteError> {
    let mut statement = conn.prepare(sql).map_err(error)?;
    let ids = statement
        .query_map([id.to_string()], |row| row.get::<_, String>(0))
        .map_err(error)?;
    ids.map(|row| crate::summary_port::parse_id(&row.map_err(error)?))
        .collect()
}

fn encode_ids(ids: &impl serde::Serialize) -> Result<String, SqliteError> {
    serde_json::to_string(ids).map_err(|source| SqliteError::Encode {
        column: "rehearsal IDs",
        source,
    })
}
