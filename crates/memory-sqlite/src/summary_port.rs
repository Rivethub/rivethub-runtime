mod window;

use crate::summary_backend as backend;
use async_trait::async_trait;
use jiff::Timestamp;
use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::HEARTBEAT_SESSION_PREFIX;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::NewSummary;
use kernel::memory::PendingConversation;
use kernel::memory::Provenance;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::SummaryWatch;
use kernel::memory::SummaryWrite;
use kernel::memory::ToolUse;
use kernel::memory::WindowMessage;
use kernel::memory::WrittenSummary;
use kernel::memory::settlement::{SettlementPage, SummaryState};
use kernel::memory::summary_backend::SummaryArtifact;
use kernel::memory::summary_backend::{SummaryIndex, SummaryRepair};
use kernel::memory::summary_backend::{SummaryIndexes, SummaryInput, SummaryRun};
use kernel::memory::summary_opening::OpeningTitle;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::summary_reference::PreparedSummaryTree;
use kernel::queue::Job;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use uuid::Uuid;
use window::load_window;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::{stored_role, transcript_metadata};
use crate::time;

#[async_trait]
impl SummaryStore for SqliteStore {
    fn summary_conversion(
        &self,
    ) -> Option<&dyn kernel::memory::summary_conversion::SummaryConversion> {
        Some(self)
    }

    async fn live_candidates(
        &self,
        after: Uuid,
        limit: i64,
    ) -> Result<Vec<(Uuid, i64)>, SummaryStoreError> {
        self.with_conn(move |conn| backend::candidates(conn, after, limit))
            .await
            .map_err(store_err)
    }

    async fn live_input(&self, id: Uuid) -> Result<Option<SummaryInput>, SummaryStoreError> {
        self.with_conn(move |conn| {
            let tx = conn.transaction().map_err(backend::error)?;
            let input = backend::input(&tx, id)?;
            tx.commit().map_err(backend::error)?;
            Ok(input)
        })
        .await
        .map_err(store_err)
    }

    async fn live_run(&self, id: Uuid) -> Result<Option<SummaryRun>, SummaryStoreError> {
        self.with_conn(move |conn| {
            let row = conn
                .query_row(
                    "SELECT id, revision, manifest FROM rh_summary_runs WHERE conversation_id = ?1",
                    [id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(backend::error)?;
            row.map(|(id, revision, manifest)| {
                Ok(SummaryRun {
                    id: parse_id(&id)?,
                    revision,
                    manifest: workspace_json(&manifest)?,
                })
            })
            .transpose()
        })
        .await
        .map_err(store_err)
    }

    async fn freeze_live_run(
        &self,
        id: Uuid,
        revision: i64,
        root: Option<Uuid>,
        manifest: SummaryArtifact,
    ) -> Result<Uuid, SummaryStoreError> {
        self.with_conn(move |conn| backend::freeze(conn, id, revision, root, &manifest))
            .await
            .map_err(store_err)
    }

    async fn live_checkpoint(
        &self,
        run: Uuid,
        key: [u8; 32],
    ) -> Result<Option<SummaryArtifact>, SummaryStoreError> {
        self.with_conn(move |conn| {
            conn.query_row("SELECT artifact FROM rh_summary_checkpoints WHERE run_id = ?1 AND artifact_key = ?2",
                params![run.to_string(), key.as_slice()], |row| row.get::<_, String>(0))
                .optional().map_err(backend::error)?.map(|value| workspace_json(&value)).transpose()
        }).await.map_err(store_err)
    }

    async fn save_live_checkpoint(
        &self,
        run: Uuid,
        revision: i64,
        key: [u8; 32],
        artifact: SummaryArtifact,
    ) -> Result<(), SummaryStoreError> {
        self.with_conn(move |conn| {
            conn.execute("INSERT INTO rh_summary_checkpoints (run_id, artifact_key, artifact)
                          SELECT id, ?2, ?3 FROM rh_summary_runs WHERE id = ?1 AND revision = ?4 ON CONFLICT DO NOTHING",
                params![run.to_string(), key.as_slice(), artifact.as_json(), revision]).map_err(backend::error)?;
            Ok(())
        }).await.map_err(store_err)
    }

    async fn publish_live(
        &self,
        tree: PreparedSummaryTree,
        indexes: SummaryIndexes,
    ) -> Result<bool, SummaryStoreError> {
        match self.replace_live_summary(tree, indexes).await {
            Ok(()) => Ok(true),
            Err(SqliteError::SummaryReference(_)) => Ok(false),
            Err(error) => Err(store_err(error)),
        }
    }

    async fn fail_live(&self, id: Uuid, revision: i64) -> Result<(), SummaryStoreError> {
        self.with_conn(move |conn| {
            conn.execute("UPDATE rh_summary_state SET failed_revision = ?2 WHERE conversation_id = ?1
                AND input_revision = ?2 AND settled_at IS NOT NULL AND summarized_through IS NOT ?2",
                params![id.to_string(), revision]).map_err(backend::error)?;
            Ok(())
        }).await.map_err(store_err)
    }

    fn live_index_key(&self) -> Option<[u8; 32]> {
        self.summary_index_key
    }

    async fn summary_state(&self, id: Uuid) -> Result<Option<SummaryState>, SummaryStoreError> {
        self.read_summary_state(id).await.map_err(store_err)
    }

    async fn summary_eligible(&self, id: Uuid, revision: i64) -> Result<bool, SummaryStoreError> {
        self.eligible_summary(id, revision).await.map_err(store_err)
    }

    async fn expire_summary_page(
        &self,
        after: Option<Uuid>,
        cutoff: Timestamp,
        now: Timestamp,
        limit: u32,
    ) -> Result<SettlementPage, SummaryStoreError> {
        self.expire_settlement(after, cutoff, now, limit)
            .await
            .map_err(store_err)
    }

    async fn repair_candidates(&self, limit: u32) -> Result<Vec<Uuid>, SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(Vec::new());
        };
        self.with_conn(move |conn| crate::summary_index::marked_indexes(conn, key, limit))
            .await
            .map_err(store_err)
    }

    async fn repair_input(&self, id: Uuid) -> Result<Option<SummaryRepair>, SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(None);
        };
        self.with_conn(move |conn| crate::summary_index::marked_index(conn, id, key))
            .await
            .map_err(store_err)
    }

    async fn repair_index(
        &self,
        expected: SummaryRepair,
        index: SummaryIndex,
    ) -> Result<(), SummaryStoreError> {
        let Some(key) = self.summary_index_key else {
            return Ok(());
        };
        self.with_conn(move |conn| {
            crate::summary_index::replace_index(conn, key, &expected, &index)
        })
        .await
        .map_err(store_err)
    }

    async fn conversation(
        &self,
        id: Uuid,
    ) -> Result<Option<ConversationWindow>, SummaryStoreError> {
        self.with_conn(move |conn| load_window(conn, id))
            .await
            .map_err(store_err)
    }

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError> {
        self.with_conn(|conn| pending_conversations(conn))
            .await
            .map_err(store_err)
    }

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError> {
        if summary.kind.is_top_down() {
            return Err(SummaryStoreError::Kind {
                kind: summary.kind.as_str().to_owned(),
            });
        }
        let (id, kind, depth) = (summary.id, summary.kind, summary.depth);
        let (written, session_key) = self
            .with_conn(move |conn| write_one(conn, &summary))
            .await
            .map_err(store_err)?;
        let told = self.summaries_written.send(WrittenSummary {
            id,
            session_key,
            kind,
            depth,
        });
        if told.is_err() {
            tracing::trace!("written summary has no listener");
        }
        Ok(written)
    }

    async fn opening_title(&self, id: Uuid) -> Result<Option<OpeningTitle>, SummaryStoreError> {
        self.with_conn(move |conn| crate::title::load(conn, &id.to_string()))
            .await
            .map_err(store_err)
    }

    async fn finish_opening_title(
        &self,
        opening: &OpeningTitle,
        title: Option<&str>,
    ) -> Result<bool, SummaryStoreError> {
        let opening = opening.clone();
        let title = title.map(str::to_owned);
        self.with_conn(move |conn| crate::title::finish(conn, &opening, title.as_deref()))
            .await
            .map_err(store_err)
    }

    async fn opening_filing(&self, id: Uuid) -> Result<Option<bool>, SummaryStoreError> {
        Self::opening_filing(self, id).await.map_err(store_err)
    }

    async fn finish_opening_filing(&self, id: Uuid) -> Result<(), SummaryStoreError> {
        Self::finish_opening_filing(self, id)
            .await
            .map_err(store_err)
    }

    async fn undelivered_openings(&self) -> Result<Vec<Job>, SummaryStoreError> {
        Self::undelivered_openings(self).await.map_err(store_err)
    }

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError> {
        let title = title.to_owned();
        self.with_conn(move |conn| crate::title::name_conversation(conn, &id.to_string(), &title))
            .await
            .map_err(store_err)
    }
}

impl SummaryWatch for SqliteStore {
    fn written(&self) -> tokio::sync::broadcast::Receiver<WrittenSummary> {
        self.summaries_written.subscribe()
    }
}

fn pending_conversations(
    conn: &rusqlite::Connection,
) -> Result<Vec<PendingConversation>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT c.id, pending.unsummarized, \
                    (SELECT max(m.created_at) FROM ros_messages m WHERE m.conversation_id = c.id) \
                      AS latest_at \
               FROM (SELECT m.conversation_id, count(*) AS unsummarized FROM ros_messages m \
                      WHERE NOT EXISTS ( \
                        SELECT 1 FROM ros_summary_sources s WHERE s.message_id = m.id) \
                      GROUP BY m.conversation_id) pending \
               JOIN ros_conversations c ON c.id = pending.conversation_id \
              WHERE c.session_key NOT LIKE ?1 \
              ORDER BY latest_at DESC, c.id",
        )
        .map_err(SqliteError::query("pending summaries"))?;
    let rows = statement
        .query_map(
            params_from_iter([Value::Text(format!("{HEARTBEAT_SESSION_PREFIX}%"))]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(SqliteError::query("pending summaries"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("pending summaries"))?;
    let mut pending = Vec::with_capacity(rows.len());
    for (id, count, latest) in rows {
        match pending_conversation(&id, count, latest.as_deref()) {
            Ok(conversation) => pending.push(conversation),
            Err(error) => tracing::warn!("skipped pending conversation {id}: {error}"),
        }
    }
    Ok(pending)
}

fn pending_conversation(
    id: &str,
    count: i64,
    latest: Option<&str>,
) -> Result<PendingConversation, SqliteError> {
    Ok(PendingConversation {
        id: parse_id(id)?,
        unsummarized: u32::try_from(count).map_err(SqliteError::Count)?,
        latest_message_at: latest
            .map(|text| time::stored(text, "created_at"))
            .transpose()?,
    })
}

fn write_one(
    conn: &mut rusqlite::Connection,
    summary: &NewSummary,
) -> Result<(SummaryWrite, String), SqliteError> {
    let count = i64::from(summary.message_count);
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("summary begin"))?;
    tx.execute(
        "INSERT INTO ros_summaries \
            (id, conversation_id, depth, content, kind, message_count, earliest_at, latest_at, \
             model, created_at, pipeline_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params_from_iter([
            Value::Text(summary.id.to_string()),
            Value::Text(summary.conversation_id.to_string()),
            Value::Integer(i64::from(summary.depth)),
            Value::Text(summary.content.clone()),
            Value::Text(summary.kind.as_str().to_owned()),
            Value::Integer(count),
            Value::Text(time::stamp(summary.earliest_at)),
            Value::Text(time::stamp(summary.latest_at)),
            Value::Text(summary.model.clone()),
            Value::Text(time::stamp(summary.created_at)),
            Value::Integer(i64::from(summary.pipeline_version)),
        ]),
    )
    .map_err(SqliteError::query("insert summary"))?;
    for source in &summary.sources {
        tx.execute(
            "INSERT INTO ros_summary_sources (summary_id, message_id, ordinal) VALUES (?1, ?2, ?3)",
            params_from_iter([
                Value::Text(summary.id.to_string()),
                Value::Text(source.message_id.to_string()),
                Value::Integer(i64::from(source.ordinal)),
            ]),
        )
        .map_err(SqliteError::query("insert summary source"))?;
    }
    for child in &summary.child_ids {
        let updated = tx
            .execute(
                "UPDATE ros_summaries SET parent_id = ?1 WHERE id = ?2 AND conversation_id = ?3",
                params_from_iter([
                    Value::Text(summary.id.to_string()),
                    Value::Text(child.to_string()),
                    Value::Text(summary.conversation_id.to_string()),
                ]),
            )
            .map_err(SqliteError::query("link summary parent"))?;
        if updated != 1 {
            return Err(SqliteError::MissingHit {
                id: child.to_string(),
            });
        }
    }
    let written = if summary.kind == SummaryKind::Leaf {
        let conversation = summary.conversation_id.to_string();
        leaf_write(&tx, &conversation)?
    } else {
        SummaryWrite::Added
    };
    let session_key: String = tx
        .query_row(
            "SELECT session_key FROM ros_conversations WHERE id = ?1",
            params_from_iter([Value::Text(summary.conversation_id.to_string())]),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("summary session"))?;
    tx.commit().map_err(SqliteError::query("summary commit"))?;
    Ok((written, session_key))
}

fn leaf_write(
    conn: &rusqlite::Connection,
    conversation: &str,
) -> Result<SummaryWrite, SqliteError> {
    let leaves: i64 = conn
        .query_row(
            "SELECT count(*) FROM ros_summaries WHERE conversation_id = ?1 AND kind = ?2",
            params_from_iter([
                Value::Text(conversation.to_owned()),
                Value::Text(SummaryKind::Leaf.as_str().to_owned()),
            ]),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("count leaves"))?;
    Ok(if leaves == 1 {
        SummaryWrite::FirstLeaf
    } else {
        SummaryWrite::Added
    })
}

pub(crate) fn parse_id(id: &str) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(id).map_err(|source| SqliteError::Query {
        operation: "summary id",
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}

fn store_err(error: SqliteError) -> SummaryStoreError {
    if let SqliteError::MissingHit { id } = error {
        return SummaryStoreError::Missing { id };
    }
    if let SqliteError::Count(_) = error {
        return SummaryStoreError::Count;
    }
    if matches!(
        error,
        SqliteError::Open { .. }
            | SqliteError::NotInitialised { .. }
            | SqliteError::SchemaBehind { .. }
            | SqliteError::Pragma(_)
            | SqliteError::UnsupportedSchema { .. }
            | SqliteError::Join(_)
    ) {
        return SummaryStoreError::Closed;
    }
    if let SqliteError::Invalid { message } = &error
        && let Some(kind) = stored_kind(message)
    {
        return SummaryStoreError::Kind { kind };
    }
    SummaryStoreError::Query {
        message: error.to_string(),
        source: Box::new(error),
    }
}

fn stored_kind(message: &str) -> Option<String> {
    let rest = message.strip_prefix("stored summary kind ")?;
    let kind = rest.strip_suffix(" is not leaf, branch, or root")?;
    Some(kind.to_owned())
}

fn workspace_json(value: &str) -> Result<SummaryArtifact, SqliteError> {
    serde_json::from_str(value).map_err(|source| SqliteError::StoredJson {
        column: "summary workspace",
        source,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::pending_conversations;

    const HEALTHY: &str = "0199e2a0-7b3c-7a00-8000-0000000000a1";
    const UNREADABLE: &str = "not-a-conversation-id";

    fn seed(conn: &Connection, conversation: &str, message: &str) {
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
             VALUES (?1, ?1, 'rivet', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
            [conversation],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'rivet', 'test', 'user', 'hello', '2026-10-10T12:00:00.000Z')",
            [message, conversation],
        )
        .unwrap();
    }

    #[test]
    fn a_conversation_that_cannot_be_read_is_skipped_with_a_warning() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        seed(&conn, HEALTHY, "0199e2a0-7b3c-7a00-8001-0000000000a1");
        seed(&conn, UNREADABLE, "0199e2a0-7b3c-7a00-8001-0000000000b2");
        let (guard, lines) = testkit::logs::recorded();
        let pending = pending_conversations(&conn).unwrap();
        drop(guard);
        let ids: Vec<String> = pending
            .iter()
            .map(|conversation| conversation.id.to_string())
            .collect();
        assert_eq!(ids, [HEALTHY]);
        let warnings = lines.lock().clone();
        let [warning] = warnings.as_slice() else {
            panic!("one warning: {warnings:?}");
        };
        assert!(
            warning.starts_with(&format!("WARN skipped pending conversation {UNREADABLE}: ")),
            "{warning}"
        );
    }
}
