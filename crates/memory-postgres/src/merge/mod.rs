mod stage;
mod table;
mod transform;

use std::collections::BTreeMap;
use std::time::Instant;

use kernel::owner::Owner;
use sqlx::PgConnection;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::PostgresMemory;
use crate::error::Error;
use table::Table;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    pub owner: Owner,
    pub moves: BTreeMap<Uuid, Owner>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub source: String,
    pub runs: u32,
    pub staged: BTreeMap<String, u64>,
    pub inserted: BTreeMap<String, u64>,
    pub kept_family: Vec<String>,
    pub replaced: Vec<String>,
    pub moved_conversations: u64,
    pub mapped_type_topics: u64,
    pub renamed_topics: u64,
    pub dropped_aliases: u64,
    pub prefixed_pages: u64,
    pub embeddings_cleared: bool,
    pub committed: bool,
    pub millis: u64,
}

impl PostgresMemory {
    pub async fn merge_store(
        &self,
        source: &Self,
        request: &MergeRequest,
        cancel: &CancellationToken,
    ) -> Result<MergeReport, Error> {
        let started = Instant::now();
        let from = source.identity().await?;
        let into = self.identity().await?;
        if (from.cluster, from.oid) == (into.cluster, into.oid) {
            return Err(Error::Merge(
                "the source and the family store are one database".into(),
            ));
        }
        let key = format!("{}:{}:{}", from.cluster, from.oid, from.name);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| Error::operation("merge", error))?;
        let mut reading = source
            .pool
            .begin()
            .await
            .map_err(|error| Error::operation("merge source", error))?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *reading)
            .await
            .map_err(|error| Error::operation("merge source snapshot", error))?;
        stage::same_schema(&mut reading, &mut tx).await?;
        let mut report = MergeReport {
            source: key.clone(),
            ..MergeReport::default()
        };
        let mut columns = Vec::with_capacity(Table::ALL.len());
        for table in Table::ALL {
            if cancel.is_cancelled() {
                return Err(Error::Merge("merge cancelled".into()));
            }
            let (rows, staged) = stage::copy(&mut reading, &mut tx, table.name()).await?;
            report.staged.insert(table.name().to_owned(), rows);
            columns.push((table, staged));
        }
        reading
            .rollback()
            .await
            .map_err(|error| Error::operation("merge source snapshot", error))?;
        report.embeddings_cleared = stage::align_embeddings(source, self, &mut tx).await?;
        stage::mark_summary_repairs(&mut tx).await?;
        transform::run(&mut tx, request, &mut report).await?;
        for (table, staged) in &columns {
            let rows = transform::insert(&mut tx, table.name(), staged).await?;
            report.inserted.insert(table.name().to_owned(), rows);
        }
        crate::dump::validate_summary_graph(&mut tx).await?;
        report.runs = record(&mut tx, &key, request, &report).await?;
        report.committed = !request.dry_run;
        if request.dry_run {
            tx.rollback()
                .await
                .map_err(|error| Error::operation("merge rollback", error))?;
        } else {
            tx.commit()
                .await
                .map_err(|error| Error::operation("merge commit", error))?;
        }
        report.millis = u64::try_from(started.elapsed().as_millis())
            .map_err(|error| Error::Merge(format!("merge time: {error}")))?;
        Ok(report)
    }
}

async fn record(
    conn: &mut PgConnection,
    key: &str,
    request: &MergeRequest,
    report: &MergeReport,
) -> Result<u32, Error> {
    let rows = serde_json::to_value(&report.inserted)
        .map_err(|error| Error::Merge(format!("merge counts: {error}")))?;
    let runs: i32 = sqlx::query_scalar(
        "INSERT INTO rh_store_merges (source, owner_user_id, first_at, last_at, rows)
         VALUES ($1, $2, now(), now(), $3)
         ON CONFLICT (source) DO UPDATE SET last_at = now(), runs = rh_store_merges.runs + 1, rows = EXCLUDED.rows
         RETURNING runs",
    )
    .bind(key)
    .bind(request.owner.as_str())
    .bind(rows)
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| Error::operation("merge record", error))?;
    sqlx::query(
        "INSERT INTO rh_merge_map (source, kind, source_id, target_id)
         SELECT $1, 'topic', source_id::text, target_id::text FROM merge_topic_map
         UNION ALL SELECT $1, 'wiki-page', old, new FROM merge_slug_map
         UNION ALL SELECT $1, 'replaced-conversation', target_id::text, source_id::text
           FROM merge_dupes WHERE source_wins
         ON CONFLICT (source, kind, source_id) DO UPDATE SET target_id = EXCLUDED.target_id",
    )
    .bind(key)
    .execute(&mut *conn)
    .await
    .map_err(|error| Error::operation("merge record", error))?;
    u32::try_from(runs).map_err(|error| Error::Merge(format!("merge runs: {error}")))
}
