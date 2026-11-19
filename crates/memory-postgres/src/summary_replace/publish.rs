use super::sql;
use kernel::memory::summary_backend::{SummaryIndex, SummaryIndexes};
use kernel::memory::summary_conversion::ConversionLease;
use std::collections::{BTreeMap, BTreeSet};

use kernel::memory::summary_reference::{
    PreparedSummaryRecord, PreparedSummaryTree, SummaryCoverage, SummaryReferenceError,
    replacement_target,
};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::{Error, PostgresMemory};

use super::{exclusive, retain};

const OLD_COVERAGE: &str = "
WITH RECURSIVE tree AS (
    SELECT id, parent_id, kind, source_first, source_last, range_verified
    FROM rh_summaries WHERE conversation_id = $1 AND owner_user_id = $2
), descendants AS (
    SELECT id AS old, id AS node FROM tree WHERE kind IN ('branch', 'root')
    UNION
    SELECT d.old, t.id FROM descendants d JOIN tree t ON t.parent_id = d.node
), membership AS (
    SELECT old, node AS source, NULL::integer AS first, NULL::integer AS last FROM descendants
    UNION
    SELECT id, id, NULL, NULL FROM tree WHERE kind IN ('leaf', 'session', 'part')
    UNION
    SELECT t.id, t.parent_id, t.source_first, t.source_last FROM tree t
    JOIN tree p ON p.id = t.parent_id AND p.kind IN ('part', 'session')
    WHERE t.kind = 'section' AND t.range_verified
      AND EXISTS (SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = p.id AND l.ordinal = t.source_first)
      AND EXISTS (SELECT 1 FROM rh_summary_sources l WHERE l.summary_id = p.id AND l.ordinal = t.source_last)
)
SELECT DISTINCT map.old, l.message_id FROM membership map
JOIN rh_summary_sources l ON l.summary_id = map.source
JOIN rh_messages m ON m.id = l.message_id AND m.conversation_id = $1 AND m.owner_user_id = $2
WHERE map.first IS NULL OR l.ordinal BETWEEN map.first AND map.last";

const INSERT_RECORD: &str = "
INSERT INTO rh_summaries (id, conversation_id, owner_user_id, parent_id, kind, depth, content,
    summary_ordinal, input_key, index_key, model, issued_part_ordinal, input_token_estimate,
    quality_flags, section_char_start, section_char_end, range_verified,
    source_first, source_last, index_needs_repair, message_count, earliest_at, latest_at,
    created_at, pipeline_version)
SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
    $18, $19, $5 <> 'section', count(*)::integer, min(m.created_at), max(m.created_at), $20, 7
FROM rh_messages m WHERE m.conversation_id = $2 AND m.owner_user_id = $3 AND m.id = ANY($21)
HAVING count(*) = cardinality($21)";

#[derive(Debug, thiserror::Error)]
pub enum ReplaceSummaryError {
    #[error(transparent)]
    Reference(SummaryReferenceError),
    #[error(transparent)]
    Store(Error),
}

impl From<Error> for ReplaceSummaryError {
    fn from(error: Error) -> Self {
        Self::Store(error)
    }
}

impl From<SummaryReferenceError> for ReplaceSummaryError {
    fn from(error: SummaryReferenceError) -> Self {
        Self::Reference(error)
    }
}

pub(super) fn query(error: sqlx::Error) -> Error {
    Error::operation("replace summary tree", error)
}

impl PostgresMemory {
    #[doc(hidden)]
    pub async fn replace_summary_tree(
        &self,
        prepared: &PreparedSummaryTree,
    ) -> Result<(), ReplaceSummaryError> {
        self.replace_tree(prepared, None, None).await
    }

    pub(crate) async fn replace_live_summary(
        &self,
        prepared: &PreparedSummaryTree,
        indexes: &SummaryIndexes,
    ) -> Result<(), ReplaceSummaryError> {
        self.replace_tree(prepared, Some(indexes), None).await
    }

    pub(crate) async fn replace_conversion_summary(
        &self,
        prepared: &PreparedSummaryTree,
        indexes: &SummaryIndexes,
        lease: ConversionLease,
    ) -> Result<(), ReplaceSummaryError> {
        self.replace_tree(prepared, Some(indexes), Some(lease))
            .await
    }

    async fn replace_tree(
        &self,
        prepared: &PreparedSummaryTree,
        indexes: Option<&SummaryIndexes>,
        conversion: Option<ConversionLease>,
    ) -> Result<(), ReplaceSummaryError> {
        prepared.validate_index_key(self.summary_index_key)?;
        for index in indexes.into_iter().flat_map(BTreeMap::values) {
            index
                .validate_width()
                .map_err(|error| Error::protocol(error.to_string()))?;
        }
        let mut tx = self.pool.begin().await.map_err(query)?;
        if let Some(lease) = conversion
            && !crate::summary_conversion::can_publish(&mut tx, lease)
                .await
                .map_err(query)?
        {
            return Err(SummaryReferenceError::Stale(prepared.conversation).into());
        }
        exclusive(&mut tx, [prepared.conversation])
            .await
            .map_err(query)?;
        validate_current(&mut tx, prepared).await?;
        if let Some(lease) = conversion
            && !crate::summary_conversion::can_publish(&mut tx, lease)
                .await
                .map_err(query)?
        {
            return Err(SummaryReferenceError::Stale(prepared.conversation).into());
        }
        if indexes.is_some() {
            let revision = prepared
                .expected_revision
                .ok_or(SummaryReferenceError::Stale(prepared.conversation))?;
            if !crate::summary_backend::eligible(&mut tx, prepared.conversation, revision)
                .await
                .map_err(query)?
            {
                return Err(SummaryReferenceError::Stale(prepared.conversation).into());
            }
            if prepared.records.is_empty() {
                if !prepared.expected_ids.is_empty() {
                    return Err(Error::protocol(
                        "empty transcript would replace an existing summary",
                    )
                    .into());
                }
                finish_revision(&mut tx, prepared, conversion.is_some()).await?;
                tx.commit().await.map_err(query)?;
                return Ok(());
            }
        }
        let mut coverage = prepared
            .coverage()
            .map_err(|error| Error::stored("summary manifest", error))?;
        let old = old_coverage(&mut tx, prepared.conversation, &prepared.owner).await?;
        let mut plan = prepared.clone();
        let retained = retain::plan(&mut tx, &mut plan, &mut coverage).await?;
        let prepared = &plan;
        stage(&mut tx, prepared, &coverage, &old, &retained).await?;
        import_deliveries(&mut tx, prepared).await?;

        write_replacement_records(&mut tx, prepared, indexes, &coverage, &retained).await?;
        sqlx::raw_sql(sql::REFERENCES)
            .execute(&mut *tx)
            .await
            .map_err(query)?;
        remove_queue_work(&mut tx).await?;
        if indexes.is_some() && conversion.is_none() {
            enqueue_deliveries(&mut tx, prepared.conversation).await?;
        }
        finish_revision(&mut tx, prepared, conversion.is_some()).await?;
        tx.commit().await.map_err(query)?;
        Ok(())
    }
}

async fn write_replacement_records(
    conn: &mut PgConnection,
    tree: &PreparedSummaryTree,
    indexes: Option<&SummaryIndexes>,
    coverage: &SummaryCoverage,
    retained: &BTreeSet<Uuid>,
) -> Result<(), ReplaceSummaryError> {
    let indexable_records: Vec<_> = tree
        .records
        .iter()
        .filter(|row| {
            row.kind.depth() > 0 && row.index_key.is_some() && !retained.contains(&row.id)
        })
        .collect();
    if !indexable_records.is_empty() {
        let keys: BTreeSet<_> = indexable_records
            .iter()
            .filter_map(|row| row.index_key)
            .collect();
        let keys: Vec<_> = keys.iter().map(<[_; 32]>::as_slice).collect();
        let old_ids: Vec<_> = tree.expected_ids.iter().copied().collect();
        sqlx::query(sql::SNAPSHOT_PASSAGES)
            .bind(old_ids)
            .bind(keys)
            .execute(&mut *conn)
            .await
            .map_err(query)?;
    }

    sqlx::query(
        "UPDATE rh_summaries s SET kind = 'root', pipeline_version = 6
         FROM summary_replacements map WHERE s.id = map.old AND s.kind = 'session'",
    )
    .execute(&mut *conn)
    .await
    .map_err(query)?;

    let mut records: Vec<_> = tree.records.iter().collect();
    records.sort_by_key(|record| std::cmp::Reverse(record.kind.depth()));
    for record in records {
        if retained.contains(&record.id) {
            retain::reparent(conn, record).await?;
        } else {
            insert_record(conn, tree, record, coverage).await?;
        }
    }
    for record in indexable_records {
        let (starts, ends): (Vec<_>, Vec<_>) = tree
            .passage_windows
            .get(&record.id)
            .into_iter()
            .flatten()
            .map(|range| (range.start, range.end))
            .unzip();
        sqlx::query(sql::REUSE_PASSAGES)
            .bind(record.id)
            .bind(starts)
            .bind(ends)
            .execute(&mut *conn)
            .await
            .map_err(query)?;
        if let Some(indexes) = indexes {
            let index = indexes
                .get(&record.id)
                .ok_or_else(|| Error::protocol("missing prepared summary index"))?;
            write_index(conn, record, index).await?;
        }
        let complete: bool = sqlx::query_scalar(
            "UPDATE rh_summaries SET index_needs_repair = NOT rh_summary_record_indexed(id)
                 WHERE id = $1 RETURNING NOT index_needs_repair",
        )
        .bind(record.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(query)?;
        if indexes.is_some() && !complete {
            return Err(Error::protocol("incomplete published summary index").into());
        }
    }
    Ok(())
}

async fn validate_current(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedSummaryTree,
) -> Result<(), ReplaceSummaryError> {
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT owner_user_id FROM rh_conversations WHERE id = $1 AND rh_scope_maintenance()",
    )
    .bind(prepared.conversation)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query)?;
    let owner = owner.ok_or(SummaryReferenceError::Gone(prepared.conversation))?;
    let revision: Option<i64> = sqlx::query_scalar(
        "SELECT input_revision FROM rh_summary_state WHERE conversation_id = $1 FOR UPDATE",
    )
    .bind(prepared.conversation)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query)?;
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM rh_summaries WHERE conversation_id = $1 ORDER BY id")
            .bind(prepared.conversation)
            .fetch_all(&mut **tx)
            .await
            .map_err(query)?;
    if owner != prepared.owner
        || revision != prepared.expected_revision
        || ids.into_iter().ne(prepared.expected_ids.iter().copied())
    {
        return Err(SummaryReferenceError::Stale(prepared.conversation).into());
    }
    Ok(())
}

pub(crate) async fn old_coverage(
    conn: &mut PgConnection,
    conversation: Uuid,
    owner: &str,
) -> Result<SummaryCoverage, Error> {
    let links: Vec<(Uuid, Uuid)> = sqlx::query_as(OLD_COVERAGE)
        .bind(conversation)
        .bind(owner)
        .fetch_all(conn)
        .await
        .map_err(query)?;
    let mut coverage = BTreeMap::<Uuid, BTreeSet<Uuid>>::new();
    for (id, message) in links {
        coverage.entry(id).or_default().insert(message);
    }
    Ok(coverage)
}

async fn stage(
    conn: &mut PgConnection,
    tree: &PreparedSummaryTree,
    coverage: &SummaryCoverage,
    old: &SummaryCoverage,
    retained: &BTreeSet<Uuid>,
) -> Result<(), Error> {
    sqlx::raw_sql(
        "CREATE TEMP TABLE summary_replacements (old uuid PRIMARY KEY, new uuid, kind text) ON COMMIT DROP;
         CREATE TEMP TABLE summary_anchors (message uuid PRIMARY KEY, anchor uuid NOT NULL) ON COMMIT DROP;
         CREATE TEMP TABLE summary_retained (id uuid PRIMARY KEY) ON COMMIT DROP;
         CREATE TEMP TABLE summary_recovery (summary_id uuid, consumer text) ON COMMIT DROP;",
    ).execute(&mut *conn).await.map_err(query)?;
    sqlx::query("INSERT INTO summary_retained SELECT unnest($1::uuid[])")
        .bind(retained.iter().copied().collect::<Vec<_>>())
        .execute(&mut *conn)
        .await
        .map_err(query)?;
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
    let mut ids = Vec::new();
    let mut targets = Vec::new();
    let mut kinds = Vec::new();
    for id in tree.expected_ids.difference(retained) {
        let target = old
            .get(id)
            .and_then(|members| replacement_target(members, candidates.iter().copied()));
        ids.push(*id);
        targets.push(target);
        kinds.push(
            target
                .map(|id| {
                    records
                        .get(&id)
                        .copied()
                        .ok_or_else(|| Error::protocol(format!("missing replacement summary {id}")))
                })
                .transpose()?,
        );
    }
    sqlx::query(
        "INSERT INTO summary_replacements SELECT * FROM unnest($1::uuid[], $2::uuid[], $3::text[])",
    )
    .bind(&ids)
    .bind(&targets)
    .bind(&kinds)
    .execute(&mut *conn)
    .await
    .map_err(query)?;
    let (messages, anchors): (Vec<_>, Vec<_>) = tree.anchors().into_iter().unzip();
    sqlx::query("INSERT INTO summary_anchors SELECT * FROM unnest($1::uuid[], $2::uuid[])")
        .bind(&messages)
        .bind(&anchors)
        .execute(conn)
        .await
        .map_err(query)?;
    Ok(())
}

async fn import_deliveries(
    conn: &mut PgConnection,
    tree: &PreparedSummaryTree,
) -> Result<(), Error> {
    if queue_present(conn).await? {
        sqlx::query(
            "INSERT INTO summary_recovery (summary_id, consumer)
             SELECT map.old, CASE WHEN j.key = 'wiki-ext-' || map.old THEN 'wiki' ELSE 'entity' END
             FROM summary_replacements map JOIN graphile_worker._private_jobs j
             ON j.key IN ('wiki-ext-' || map.old, 'entity-ext-' || map.old)
             WHERE j.attempts < j.max_attempts AND NOT EXISTS (
                 SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = j.key)",
        )
        .execute(&mut *conn)
        .await
        .map_err(query)?;
    }
    sqlx::query(sql::DELIVERIES)
        .bind(tree.conversation)
        .bind(tree.historical)
        .bind(&tree.owner)
        .execute(&mut *conn)
        .await
        .map_err(query)?;
    sqlx::query(
        "UPDATE rh_summary_deliveries d SET anchor_id = NULL, historical_backlog = true
         FROM summary_replacements r
         WHERE d.anchor_id = r.old AND NOT EXISTS
           (SELECT 1 FROM summary_anchors current WHERE current.message = d.message_id)",
    )
    .execute(&mut *conn)
    .await
    .map_err(query)?;
    sqlx::query(
        "UPDATE rh_summary_deliveries d SET anchor_id = a.anchor
         FROM summary_anchors a, summary_replacements r
         WHERE d.anchor_id = r.old AND a.message = d.message_id",
    )
    .execute(conn)
    .await
    .map_err(query)?;
    Ok(())
}

async fn insert_record(
    conn: &mut PgConnection,
    tree: &PreparedSummaryTree,
    record: &PreparedSummaryRecord,
    coverage: &SummaryCoverage,
) -> Result<(), Error> {
    let members: Vec<_> = coverage[&record.id].iter().copied().collect();
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
    let written = sqlx::query(INSERT_RECORD)
        .bind(record.id)
        .bind(tree.conversation)
        .bind(&tree.owner)
        .bind(record.parent)
        .bind(record.kind.as_str())
        .bind(record.kind.depth())
        .bind(&record.content)
        .bind(record.ordinal)
        .bind(record.input_key.as_slice())
        .bind(record.index_key.as_ref().map(<[_; 32]>::as_slice))
        .bind(&record.model)
        .bind(record.issued_part_ordinal)
        .bind(record.input_tokens.map(i64::try_from).transpose()?)
        .bind(record.quality_flags)
        .bind(record.section.as_ref().map(|bounds| bounds.char_start))
        .bind(record.section.as_ref().map(|bounds| bounds.char_end))
        .bind(record.section.as_ref().map(|bounds| bounds.verified))
        .bind(first)
        .bind(last)
        .bind(crate::time::BindTime::bind_time(tree.created_at))
        .bind(&members)
        .execute(&mut *conn)
        .await
        .map_err(query)?;
    if written.rows_affected() != 1 {
        return Err(Error::DumpMessage(
            "prepared summary references a message outside its conversation".into(),
        ));
    }
    for entry in &record.sources {
        sqlx::query("INSERT INTO rh_summary_sources (summary_id, message_id, ordinal, render_key) VALUES ($1, $2, $3, $4)")
            .bind(record.id).bind(entry.message).bind(entry.ordinal)
            .bind(entry.render_key.as_ref().map(<[_; 32]>::as_slice))
            .execute(&mut *conn).await.map_err(query)?;
    }
    sqlx::query(
        "SELECT pg_notify('rh_summary_written', json_build_object(
           'id', $1::uuid, 'sessionKey', c.session_key, 'kind', $2::text, 'depth', $3::int)::text)
         FROM rh_conversations c WHERE c.id = $4",
    )
    .bind(record.id)
    .bind(record.kind.as_str())
    .bind(record.kind.depth())
    .bind(tree.conversation)
    .execute(conn)
    .await
    .map_err(query)?;
    Ok(())
}

async fn remove_queue_work(conn: &mut PgConnection) -> Result<(), Error> {
    sqlx::raw_sql(
        "DELETE FROM rh_job_failures f USING summary_replacements r
          WHERE f.job_key IN ('wiki-ext-' || r.old, 'entity-ext-' || r.old, 'embed-ros_summaries-' || r.old);
         DELETE FROM rh_flagged_jobs f USING summary_replacements r
          WHERE f.payload->>'summaryId' = r.old::text OR
                (f.payload->>'targetTable' = 'ros_summaries' AND f.payload->>'targetId' = r.old::text);",
    ).execute(&mut *conn).await.map_err(query)?;
    if queue_present(conn).await? {
        sqlx::query(
            "DELETE FROM graphile_worker._private_jobs j USING summary_replacements r
             WHERE j.locked_at IS NULL AND (j.payload->>'summaryId' = r.old::text OR
                 (j.payload->>'targetTable' = 'ros_summaries' AND j.payload->>'targetId' = r.old::text))",
        ).execute(conn).await.map_err(query)?;
    }
    Ok(())
}

async fn queue_present(conn: &mut PgConnection) -> Result<bool, Error> {
    sqlx::query_scalar("SELECT to_regclass('graphile_worker._private_jobs') IS NOT NULL")
        .fetch_one(conn)
        .await
        .map_err(query)
}

async fn finish_revision(
    conn: &mut PgConnection,
    prepared: &PreparedSummaryTree,
    conversion: bool,
) -> Result<(), Error> {
    if conversion {
        sqlx::query(
            "UPDATE rh_summary_deliveries SET anchor_id = NULL, historical_backlog = true
                     WHERE conversation_id = $1 AND completed_version IS NULL",
        )
        .bind(prepared.conversation)
        .execute(&mut *conn)
        .await
        .map_err(query)?;
        sqlx::query("UPDATE rh_summary_conversion_items SET state = 'complete', reservation_expires_at = NULL
                     WHERE conversation_id = $1")
            .bind(prepared.conversation).execute(&mut *conn).await.map_err(query)?;
    }
    sqlx::query("DELETE FROM rh_summary_runs WHERE conversation_id = $1")
        .bind(prepared.conversation)
        .execute(&mut *conn)
        .await
        .map_err(query)?;
    if let Some(revision) = prepared.expected_revision {
        sqlx::query("UPDATE rh_summary_state SET summarized_through = $2, failed_revision = NULL WHERE conversation_id = $1")
                .bind(prepared.conversation).bind(revision).execute(&mut *conn).await.map_err(query)?;
    }
    Ok(())
}

async fn write_index(
    conn: &mut PgConnection,
    row: &PreparedSummaryRecord,
    index: &SummaryIndex,
) -> Result<(), Error> {
    for (ordinal, passage) in index.passages.iter().enumerate() {
        let ordinal =
            i32::try_from(ordinal).map_err(|error| Error::stored("passage ordinal", error))?;
        let vector = pgvector::HalfVector::from_f32_slice(&passage.vector);
        let written = sqlx::query(
            "UPDATE rh_summary_passages SET embedding = $3, earliest_at = $4, latest_at = $5
             WHERE record_id = $1 AND ordinal = $2",
        )
        .bind(row.id)
        .bind(ordinal)
        .bind(vector)
        .bind(crate::time::BindTime::bind_time(passage.earliest_at))
        .bind(crate::time::BindTime::bind_time(passage.latest_at))
        .execute(&mut *conn)
        .await
        .map_err(query)?
        .rows_affected();
        if written != 1 {
            return Err(Error::protocol("missing prepared summary passage"));
        }
    }
    Ok(())
}

async fn enqueue_deliveries(conn: &mut PgConnection, conversation: Uuid) -> Result<(), Error> {
    if !queue_present(conn).await? {
        return Ok(());
    }
    sqlx::query(
        "WITH candidates AS (
             SELECT DISTINCT anchor_id, consumer,
                 CASE consumer WHEN 'wiki' THEN 'wiki-ext-' ELSE 'entity-ext-' END || anchor_id AS key
             FROM rh_summary_deliveries WHERE conversation_id = $1 AND anchor_id IS NOT NULL
         )
         SELECT graphile_worker.add_job(
             CASE consumer WHEN 'wiki' THEN 'extract-wiki' ELSE 'extract-entities' END,
             json_build_object('summaryId', anchor_id, 'conversationId', $1::uuid),
             max_attempts := 2, job_key := pending.key, priority := -10, job_key_mode := 'preserve_run_at'
         ) FROM candidates pending
         WHERE NOT EXISTS (SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = pending.key)
           AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = pending.key)"
    ).bind(conversation).execute(conn).await.map_err(query)?;
    Ok(())
}
