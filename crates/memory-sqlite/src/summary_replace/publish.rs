use super::sql;
use kernel::memory::summary_conversion::ConversionLease;
use kernel::memory::summary_reference::SummaryConsumer;
use kernel::memory::{WrittenSummary, summary_backend::SummaryIndexes};
use kernel::queue::FixedJob;
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
        prepared: PreparedSummaryTree,
    ) -> Result<(), SqliteError> {
        self.replace_tree(prepared, None, None).await.map(drop)
    }

    pub(crate) async fn replace_live_summary(
        &self,
        prepared: PreparedSummaryTree,
        indexes: SummaryIndexes,
    ) -> Result<(), SqliteError> {
        let written = self.replace_tree(prepared, Some(indexes), None).await?;
        self.notify_summary_publication(written);
        Ok(())
    }

    pub(crate) async fn replace_conversion_summary(
        &self,
        prepared: PreparedSummaryTree,
        indexes: SummaryIndexes,
        lease: ConversionLease,
    ) -> Result<(), SqliteError> {
        let written = self
            .replace_tree(prepared, Some(indexes), Some(lease))
            .await?;
        self.notify_summary_publication(written);
        Ok(())
    }

    fn notify_summary_publication(&self, written: Vec<WrittenSummary>) {
        if !written.is_empty() {
            self.notifications.send_replace(());
        }
        for notice in written {
            drop(self.summaries_written.send(notice));
        }
    }

    async fn replace_tree(
        &self,
        mut prepared: PreparedSummaryTree,
        indexes: Option<SummaryIndexes>,
        conversion: Option<ConversionLease>,
    ) -> Result<Vec<WrittenSummary>, SqliteError> {
        prepared.validate_index_key(self.summary_index_key)?;
        self.with_conn(move |conn| {
            let tx = immediate(conn).map_err(failure)?;
            validate_current(&tx, &prepared)?;
            if let Some(lease) = conversion
                && !crate::summary_conversion::can_publish(&tx, lease)?
            {
                return Err(SummaryReferenceError::Stale(prepared.conversation).into());
            }
            if indexes.is_some() {
                let revision = prepared.expected_revision.ok_or(SummaryReferenceError::Stale(prepared.conversation))?;
                if !crate::summary_backend::eligible(&tx, prepared.conversation, revision)? {
                    return Err(SummaryReferenceError::Stale(prepared.conversation).into());
                }
                if prepared.records.is_empty() {
                    if !prepared.expected_ids.is_empty() {
                        return Err(SqliteError::Invalid { message: "empty transcript would replace an existing summary".into() });
                    }
                    finish_revision(&tx, &prepared, conversion.is_some())?;
                    tx.commit().map_err(failure)?;
                    return Ok(Vec::new());
                }
            }
            let mut coverage = prepared.coverage().map_err(|error| SqliteError::Invalid { message: error.to_string() })?;
            let old = old_coverage(&tx, prepared.conversation)?;
            let retained = retain::plan(&tx, &mut prepared, &mut coverage)?;
            stage(&tx, &prepared, &coverage, &old, &retained)?;
            retain::reparent(&tx, &prepared, &retained)?;
            tx.execute(
                sql::DELIVERIES,
                params![prepared.conversation.to_string(), prepared.historical],
            ).map_err(failure)?;
            tx.execute_batch(sql::REUSE_VECTORS).map_err(failure)?;
            tx.execute(sql::REBIND_DELIVERIES, [prepared.conversation.to_string()]).map_err(failure)?;
            tx.execute_batch(sql::BEFORE_DELETE).map_err(failure)?;

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
            let written = if let Some(indexes) = indexes {
                if conversion.is_none() {
                    enqueue_deliveries(&tx, &prepared)?;
                }
                for (id, index) in indexes {
                    index.validate_width().map_err(|error| SqliteError::Invalid { message: error.to_string() })?;
                    tx.execute("UPDATE ros_summaries SET embedding = ?2, index_needs_repair = 0 WHERE id = ?1",
                        params![id.to_string(), crate::vector::pack(&index.pooled)]).map_err(failure)?;
                }
                let session_key: String = tx.query_row("SELECT session_key FROM ros_conversations WHERE id = ?1",
                    [prepared.conversation.to_string()], |row| row.get(0)).map_err(failure)?;
                prepared.records.iter().filter(|record| !retained.contains(&record.id)).map(|record| WrittenSummary {
                    id: record.id, session_key: session_key.clone(), kind: record.kind, depth: record.kind.depth(),
                }).collect()
            } else {
                Vec::new()
            };
            tx.execute_batch(include_str!("entity_references.sql")).map_err(failure)?;
            tx.execute_batch(sql::REFERENCES).map_err(failure)?;
            finish_revision(&tx, &prepared, conversion.is_some())?;
            tx.commit().map_err(failure)?;
            Ok(written)
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
    if owner.as_deref().unwrap_or_default() != tree.owner
        || revision != tree.expected_revision
        || ids
            .into_iter()
            .ne(tree.expected_ids.iter().map(Uuid::to_string))
    {
        return Err(SummaryReferenceError::Stale(tree.conversation).into());
    }
    Ok(())
}

pub(crate) fn old_coverage(
    conn: &Connection,
    conversation: Uuid,
) -> Result<SummaryCoverage, SqliteError> {
    let mut statement = conn.prepare(sql::COVERAGE).map_err(failure)?;
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

pub(crate) fn stored_uuid(value: String) -> Result<Uuid, SqliteError> {
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
        let kind = target
            .map(|id| {
                records
                    .get(&id)
                    .copied()
                    .ok_or_else(|| SqliteError::Invalid {
                        message: format!("missing replacement summary {id}"),
                    })
            })
            .transpose()?;
        moves
            .execute(params![
                id.to_string(),
                target.map(|id| id.to_string()),
                kind
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
                record
                    .input_tokens
                    .map(i64::try_from)
                    .transpose()
                    .map_err(SqliteError::Count)?,
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

fn finish_revision(
    conn: &Connection,
    prepared: &PreparedSummaryTree,
    conversion: bool,
) -> Result<(), SqliteError> {
    if conversion {
        conn.execute(
            "UPDATE ros_summary_deliveries SET anchor_id = NULL, historical_backlog = 1
                      WHERE conversation_id = ?1 AND completed_version IS NULL",
            [prepared.conversation.to_string()],
        )
        .map_err(failure)?;
        conn.execute("UPDATE rh_summary_conversion_items SET state = 'complete', reservation_expires_at = NULL
                      WHERE conversation_id = ?1",
                     [prepared.conversation.to_string()]).map_err(failure)?;
    }
    conn.execute(
        "DELETE FROM rh_summary_runs WHERE conversation_id = ?1",
        [prepared.conversation.to_string()],
    )
    .map_err(failure)?;
    if let Some(revision) = prepared.expected_revision {
        conn.execute(
                    "UPDATE rh_summary_state SET summarized_through = ?2, failed_revision = NULL WHERE conversation_id = ?1",
                    params![prepared.conversation.to_string(), revision],
                ).map_err(failure)?;
    }
    Ok(())
}

fn enqueue_deliveries(
    conn: &Connection,
    prepared: &PreparedSummaryTree,
) -> Result<(), SqliteError> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Delivery<'a> {
        summary_id: &'a str,
        conversation_id: Uuid,
    }
    let mut statement = conn
        .prepare(
            "WITH candidates AS (
                SELECT DISTINCT anchor_id, consumer,
                    CASE consumer WHEN 'wiki' THEN 'wiki-ext-' ELSE 'entity-ext-' END || anchor_id AS key
                FROM ros_summary_deliveries WHERE conversation_id = ?1 AND anchor_id IS NOT NULL
             )
             SELECT anchor_id, consumer, key FROM candidates c
             WHERE NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = c.key)",
        )
        .map_err(failure)?;
    let anchors = statement
        .query_map([prepared.conversation.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for (anchor, consumer, key) in anchors {
        let payload = Delivery {
            summary_id: &anchor,
            conversation_id: prepared.conversation,
        };
        let payload =
            serde_json::to_string(&payload).map_err(|source| SqliteError::StoredJson {
                column: "summary delivery payload",
                source,
            })?;
        let consumer: SummaryConsumer =
            consumer.parse().map_err(|source| SqliteError::Invalid {
                message: format!("summary delivery consumer: {source}"),
            })?;
        let task = match consumer {
            SummaryConsumer::Wiki => FixedJob::ExtractWiki,
            SummaryConsumer::Entity => FixedJob::ExtractEntities,
        };
        let mut job = crate::queue::JobInsert::new(task.to_string(), payload, prepared.created_at);
        job.key = Some(key);
        job.max_attempts = 2;
        job.priority = -10;
        crate::queue::enqueue_in(conn, &job)?;
    }
    Ok(())
}
