use std::collections::BTreeMap;

use crate::search::MemorySearchHit;
use chrono::{DateTime, Utc};
use kernel::memory::MemoryEntry;
use kernel::memory::{MemoryHitSource, SummaryKind};
use kernel::memory::{MemoryStoreError, SearchQuery, SummaryDetail, SummaryExcerpt};
use sqlx::types::Json;
use uuid::Uuid;

use crate::{Error, PostgresMemory, read::MessageEntryRow};

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    kind: Json<SummaryKind>,
    conversation_id: Uuid,
    earliest_at: Option<DateTime<Utc>>,
    latest_at: Option<DateTime<Utc>>,
    import_settings: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ChildRow {
    parent_id: Uuid,
    kind: Json<SummaryKind>,
    content: String,
}

#[derive(sqlx::FromRow)]
struct SourceRow {
    summary_id: Uuid,
    #[sqlx(flatten)]
    entry: MessageEntryRow,
}

impl PostgresMemory {
    pub(crate) async fn summary_details(
        &self,
        hits: &[MemorySearchHit],
        query: &SearchQuery,
    ) -> Result<BTreeMap<String, SummaryDetail>, MemoryStoreError> {
        let ids = crate::search::hit_uuids(hits, MemoryHitSource::Summary)?;
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let expand = query.expand != Some(false);
        let (rows, tags, mut children, mut sources) = tokio::try_join!(
            self.summary_rows(&ids),
            crate::tags::summary_conversation_tags(&self.pool, &ids),
            self.summary_children(&ids, expand),
            self.summary_sources(&ids, query, expand),
        )?;
        rows.into_iter()
            .map(|row| {
                let provenance = row
                    .import_settings
                    .as_deref()
                    .map(|text| {
                        serde_json::from_str::<wire::JsonObject>(text)
                            .map(|settings| kernel::memory::Provenance::from_settings(&settings))
                            .map_err(|source| Error::stored("settings", source))
                    })
                    .transpose()?;
                let detail = SummaryDetail {
                    kind: row.kind.0,
                    earliest_at: row.earliest_at,
                    latest_at: row.latest_at,
                    tags: tags.get(&row.conversation_id).cloned().unwrap_or_default(),
                    children: children.remove(&row.id).unwrap_or_default(),
                    sources: sources.remove(&row.id).unwrap_or_default(),
                    provenance,
                };
                Ok((row.id.to_string(), detail))
            })
            .collect()
    }

    async fn summary_rows(&self, ids: &[Uuid]) -> Result<Vec<SummaryRow>, MemoryStoreError> {
        let rows = sqlx::query_as("SELECT s.id, to_json(s.kind) AS kind, s.conversation_id, s.earliest_at, s.latest_at,
                    CASE WHEN c.session_key LIKE 'import:%' THEN COALESCE(c.settings::text, '{}') END AS import_settings
               FROM rh_summaries s LEFT JOIN rh_conversations c ON c.id = s.conversation_id WHERE s.id = ANY($1)")
            .bind(ids).fetch_all(&self.pool).await.map_err(|source| Error::operation("summary detail", source))?;
        Ok(rows)
    }

    async fn summary_children(
        &self,
        ids: &[Uuid],
        expand: bool,
    ) -> Result<BTreeMap<Uuid, Vec<SummaryExcerpt>>, MemoryStoreError> {
        if !expand {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<ChildRow> = sqlx::query_as("SELECT parent_id, to_json(kind) AS kind, content FROM rh_summaries WHERE parent_id = ANY($1) ORDER BY parent_id, earliest_at, id")
            .bind(ids).fetch_all(&self.pool).await.map_err(|source| Error::operation("summary children", source))?;
        let mut grouped: BTreeMap<Uuid, Vec<SummaryExcerpt>> = BTreeMap::new();
        for row in rows {
            grouped
                .entry(row.parent_id)
                .or_default()
                .push(SummaryExcerpt {
                    kind: row.kind.0,
                    content: row.content,
                });
        }
        Ok(grouped)
    }

    async fn summary_sources(
        &self,
        ids: &[Uuid],
        query: &SearchQuery,
        expand: bool,
    ) -> Result<BTreeMap<Uuid, Vec<MemoryEntry>>, MemoryStoreError> {
        if !expand {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<SourceRow> = sqlx::query_as(
            "SELECT wanted.id AS summary_id, picked.*
             FROM unnest($1::uuid[]) AS wanted(id)
             CROSS JOIN LATERAL (
                SELECT m.id::text AS id, c.session_key, m.agent, m.channel, m.role AS role_text,
                       m.content, m.tool_name, m.tool_args::text AS tool_args, m.tool_result,
                       m.metadata::text AS metadata, m.created_at, m.id AS message_id
                FROM (SELECT DISTINCT ss.message_id FROM rh_summary_sources ss
                       WHERE ss.summary_id = ANY (ARRAY(SELECT wanted.id UNION SELECT id FROM rh_summaries WHERE parent_id = wanted.id))) sources
                JOIN rh_messages m ON m.id = sources.message_id
                JOIN rh_conversations c ON c.id = m.conversation_id
                WHERE m.role <> 'tool' AND length(m.content) > 50
                AND ($2::text IS NULL OR m.agent = $2)
                AND ($3::timestamptz IS NULL OR m.created_at >= $3)
                AND ($4::timestamptz IS NULL OR m.created_at < $4)
                ORDER BY m.created_at, m.id LIMIT 200) picked
             ORDER BY wanted.id, picked.created_at, picked.message_id")
            .bind(ids).bind(&query.agent).bind(query.since).bind(query.before)
            .fetch_all(&self.pool).await.map_err(|source| Error::operation("summary source messages", source))?;
        let mut grouped: BTreeMap<Uuid, Vec<MessageEntryRow>> = BTreeMap::new();
        for row in rows {
            grouped.entry(row.summary_id).or_default().push(row.entry);
        }
        Ok(grouped
            .into_iter()
            .map(|(id, rows)| (id, crate::read::readable_entries(rows)))
            .collect())
    }
}
