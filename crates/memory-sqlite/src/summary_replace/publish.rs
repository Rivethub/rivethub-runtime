use std::collections::{BTreeMap, BTreeSet};

use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryCoverage, SummaryReferenceError,
    replacement_target,
};
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

use crate::{SqliteError, SqliteStore, time};

use super::{immediate, retain};

const INSERT_RECORD: &str = "
INSERT INTO ros_summaries (id, conversation_id, parent_id, kind, depth, content,
    summary_ordinal, input_key, index_key, model, issued_part_ordinal, input_token_estimate,
    quality_flags, section_char_start, section_char_end, range_verified,
    source_first, source_last, index_needs_repair, message_count, earliest_at, latest_at,
    created_at, pipeline_version)
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
    ?17, ?18, ?4 <> 'section', count(*), min(m.created_at), max(m.created_at), ?19, 7
FROM ros_messages m WHERE m.conversation_id = ?2 AND m.id IN (SELECT value FROM json_each(?20))
HAVING count(*) = json_array_length(?20)";

impl SqliteStore {
    #[doc(hidden)]
    pub async fn replace_summary_tree(
        &self,
        mut prepared: PreparedSummaryTree,
    ) -> Result<(), SqliteError> {
        prepared.validate_index_key(self.summary_index_key)?;
        let mut coverage = prepared.coverage().map_err(|error| SqliteError::Invalid {
            message: error.to_string(),
        })?;
        self.with_conn(move |conn| {
            let tx = immediate(conn).map_err(failure)?;
            validate_current(&tx, &prepared)?;
            let old = old_coverage(&tx, prepared.conversation)?;
            let retained = retain::plan(&tx, &mut prepared, &mut coverage)?;
            stage(&tx, &prepared, &coverage, &old, &retained)?;
            retain::reparent(&tx, &prepared, &retained)?;
            tx.execute(
                include_str!("deliveries.sql"),
                params![prepared.conversation.to_string(), prepared.historical],
            ).map_err(failure)?;
            tx.execute_batch(include_str!("reuse_vectors.sql")).map_err(failure)?;
            tx.execute_batch(include_str!("before_delete.sql")).map_err(failure)?;

            let mut records: Vec<_> = prepared.records.iter().collect();
            records.sort_by_key(|record| std::cmp::Reverse(record.kind.depth()));
            for record in records {
                if !retained.contains(&record.id) {
                    insert_record(&tx, &prepared, record, &coverage)?;
                }
            }

            tx.execute_batch(
                "UPDATE ros_summaries AS dest
                 SET (embedding, index_needs_repair) = (
                     SELECT old.embedding, 0 FROM summary_old_vectors old
                     WHERE old.content = dest.content AND old.index_key = dest.index_key
                     ORDER BY old.id LIMIT 1)
                 WHERE dest.conversation_id = (SELECT conversation_id FROM summary_old_vectors LIMIT 1)
                   AND dest.kind IN ('session', 'part')
                   AND dest.id NOT IN (SELECT id FROM summary_retained)
                   AND EXISTS (SELECT 1 FROM summary_old_vectors old
                       WHERE old.content = dest.content AND old.index_key = dest.index_key);
                 DROP TABLE summary_old_vectors;",
            ).map_err(failure)?;
            tx.execute_batch(include_str!("references.sql")).map_err(failure)?;
            tx.execute("DELETE FROM rh_summary_runs WHERE conversation_id = ?1",
                [prepared.conversation.to_string()]).map_err(failure)?;
            if let Some(revision) = prepared.expected_revision {
                tx.execute(
                    "UPDATE rh_summary_state SET summarized_through = ?2, failed_revision = NULL WHERE conversation_id = ?1",
                    params![prepared.conversation.to_string(), revision],
                ).map_err(failure)?;
            }
            tx.commit().map_err(failure)
        }).await
    }
}

pub(super) fn failure(error: rusqlite::Error) -> SqliteError {
    SqliteError::query("replace summary tree")(error)
}

fn validate_current(conn: &Connection, tree: &PreparedSummaryTree) -> Result<(), SqliteError> {
    let id = tree.conversation.to_string();
    let owner: Option<Option<String>> = conn
        .query_row(
            "SELECT owner_user_id FROM ros_conversations WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?;
    let owner = owner.ok_or(SummaryReferenceError::Gone(tree.conversation))?;
    let revision: Option<i64> = conn
        .query_row(
            "SELECT input_revision FROM rh_summary_state WHERE conversation_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?;
    let ids = conn
        .prepare("SELECT id FROM ros_summaries WHERE conversation_id = ?1 ORDER BY id")
        .and_then(|mut stmt| {
            stmt.query_map([&id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failure)?;
    if owner.as_deref() != Some(tree.owner.as_str())
        || revision != tree.expected_revision
        || ids
            .into_iter()
            .ne(tree.expected_ids.iter().map(Uuid::to_string))
    {
        return Err(SummaryReferenceError::Stale(tree.conversation).into());
    }
    Ok(())
}

fn old_coverage(conn: &Connection, conversation: Uuid) -> Result<SummaryCoverage, SqliteError> {
    let mut statement = conn
        .prepare(include_str!("coverage.sql"))
        .map_err(failure)?;
    let mut matches = statement
        .query([conversation.to_string()])
        .map_err(failure)?;
    let mut coverage = BTreeMap::<Uuid, BTreeSet<Uuid>>::new();
    while let Some(row) = matches.next().map_err(failure)? {
        let id = stored_uuid(row.get(0).map_err(failure)?)?;
        let message = stored_uuid(row.get(1).map_err(failure)?)?;
        coverage.entry(id).or_default().insert(message);
    }
    Ok(coverage)
}

pub(super) fn stored_uuid(value: String) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(&value).map_err(|_| SqliteError::StoredValue {
        column: "summary coverage ID",
        value,
    })
}

fn stage(
    conn: &Connection,
    tree: &PreparedSummaryTree,
    coverage: &SummaryCoverage,
    old: &SummaryCoverage,
    retained: &BTreeSet<Uuid>,
) -> Result<(), SqliteError> {
    conn.execute_batch(
        "CREATE TEMP TABLE summary_replacements (old text PRIMARY KEY, new text, kind text);
         CREATE TEMP TABLE summary_retained (id text PRIMARY KEY);
         CREATE TEMP TABLE summary_anchors (message text PRIMARY KEY, anchor text NOT NULL);",
    )
    .map_err(failure)?;
    let mut kept = conn
        .prepare("INSERT INTO summary_retained VALUES (?1)")
        .map_err(failure)?;
    for id in retained {
        kept.execute([id.to_string()]).map_err(failure)?;
    }
    let candidates: Vec<_> = tree
        .records
        .iter()
        .filter(|record| {
            record
                .section
                .as_ref()
                .is_none_or(|section| section.verified)
        })
        .map(|record| (record.id, &coverage[&record.id]))
        .collect();
    let records: BTreeMap<_, _> = tree
        .records
        .iter()
        .map(|record| (record.id, record.kind.as_str()))
        .collect();
    let mut moves = conn
        .prepare("INSERT INTO summary_replacements VALUES (?1, ?2, ?3)")
        .map_err(failure)?;
    for id in tree.expected_ids.difference(retained) {
        let target = old
            .get(id)
            .and_then(|members| replacement_target(members, candidates.iter().copied()));
        moves
            .execute(params![
                id.to_string(),
                target.map(|id| id.to_string()),
                target.map(|id| records[&id])
            ])
            .map_err(failure)?;
    }
    let mut deliveries = conn
        .prepare("INSERT INTO summary_anchors VALUES (?1, ?2)")
        .map_err(failure)?;
    for (message, anchor) in tree.anchors() {
        deliveries
            .execute(params![message.to_string(), anchor.to_string()])
            .map_err(failure)?;
    }
    Ok(())
}

fn insert_record(
    conn: &Connection,
    tree: &PreparedSummaryTree,
    record: &PreparedSummaryRecord,
    coverage: &SummaryCoverage,
) -> Result<(), SqliteError> {
    let members =
        serde_json::to_string(&coverage[&record.id]).map_err(|source| SqliteError::Encode {
            column: "summary coverage",
            source,
        })?;
    let first = record
        .section
        .as_ref()
        .map(|bounds| bounds.first)
        .or_else(|| record.sources.iter().map(|entry| entry.ordinal).min());
    let last = record
        .section
        .as_ref()
        .map(|bounds| bounds.last)
        .or_else(|| record.sources.iter().map(|entry| entry.ordinal).max());
    let written = conn
        .execute(
            INSERT_RECORD,
            params![
                record.id.to_string(),
                tree.conversation.to_string(),
                record.parent.map(|id| id.to_string()),
                record.kind.as_str(),
                record.kind.depth(),
                record.content,
                record.ordinal,
                record.input_key.as_slice(),
                record.index_key.as_ref().map(<[_; 32]>::as_slice),
                record.model,
                record.issued_part_ordinal,
                record.input_tokens,
                record.quality_flags,
                record.section.as_ref().map(|bounds| bounds.char_start),
                record.section.as_ref().map(|bounds| bounds.char_end),
                record.section.as_ref().map(|bounds| bounds.verified),
                first,
                last,
                time::stamp(tree.created_at),
                members,
            ],
        )
        .map_err(failure)?;
    if written != 1 {
        return Err(SqliteError::Invalid {
            message: "prepared summary references a message outside its conversation".to_owned(),
        });
    }
    let mut sources = conn.prepare(
        "INSERT INTO ros_summary_sources (summary_id, message_id, ordinal, render_key) VALUES (?1, ?2, ?3, ?4)",
    ).map_err(failure)?;
    for entry in &record.sources {
        sources
            .execute(params![
                record.id.to_string(),
                entry.message.to_string(),
                entry.ordinal,
                entry.render_key.as_ref().map(<[_; 32]>::as_slice)
            ])
            .map_err(failure)?;
    }
    Ok(())
}
