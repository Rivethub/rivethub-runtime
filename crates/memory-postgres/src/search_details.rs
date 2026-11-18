use jiff::Timestamp;
use std::collections::BTreeMap;

use crate::search::{MemorySearchHit, SearchRead, hit_uuids, passage_ids, whole_summary_ids};
use chrono::{DateTime, Utc};
use kernel::memory::MemoryEntry;
use kernel::memory::{MemoryHitSource, PassageSection, SummaryKind};
use kernel::memory::{MemoryStoreError, SearchQuery, SummaryDetail, SummaryExcerpt};
use sqlx::types::Json;
use uuid::Uuid;

use crate::{Error, read::MessageEntryRow};

#[derive(sqlx::FromRow)]
struct SummaryRow {
    id: Uuid,
    kind: Json<SummaryKind>,
    record_id: Uuid,
    conversation_id: Uuid,
    #[sqlx(try_from = "crate::time::OptionalTimestamp")]
    earliest_at: Option<Timestamp>,
    #[sqlx(try_from = "crate::time::OptionalTimestamp")]
    latest_at: Option<Timestamp>,
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

impl SearchRead<'_> {
    pub(crate) async fn summary_details(
        &mut self,
        hits: &[MemorySearchHit],
        query: &SearchQuery,
    ) -> Result<BTreeMap<String, SummaryDetail>, MemoryStoreError> {
        let ids = whole_summary_ids(hits)?;
        let passages = passage_ids(hits)?;
        if ids.is_empty() && passages.is_empty() {
            return Ok(BTreeMap::new());
        }
        let expand = query.expand != Some(false);
        let owners = hit_uuids(hits, MemoryHitSource::Summary)?;
        let rows = self.summary_rows(&ids, &passages).await?;
        let tags = crate::tags::summary_conversation_tags(&mut self.tx, &owners).await?;
        let mut children = self.summary_children(&ids, expand).await?;
        let mut sources = self.summary_sources(&ids, query, expand).await?;
        let mut sections = self.passage_sections(&passages).await?;
        if expand {
            sources.extend(self.passage_sources(&passages, query).await?);
        }
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
                    children: children.remove(&row.record_id).unwrap_or_default(),
                    sources: sources.remove(&row.id).unwrap_or_default(),
                    sections: sections.remove(&row.id).unwrap_or_default(),
                    provenance,
                };
                Ok((row.id.to_string(), detail))
            })
            .collect()
    }

    async fn summary_rows(
        &mut self,
        ids: &[Uuid],
        passages: &[Uuid],
    ) -> Result<Vec<SummaryRow>, MemoryStoreError> {
        let rows = sqlx::query_as(
            "SELECT hit.id, s.id AS record_id, to_json(s.kind) AS kind, s.conversation_id, hit.earliest_at, hit.latest_at,
                    CASE WHEN c.session_key LIKE 'import:%' THEN COALESCE(c.settings::text, '{}') END AS import_settings
             FROM (
                SELECT id, id AS record_id, earliest_at, latest_at FROM rh_summaries WHERE id = ANY($1)
                UNION ALL
                SELECT id, record_id, earliest_at, latest_at FROM rh_summary_passages WHERE id = ANY($2)
             ) hit JOIN rh_summaries s ON s.id = hit.record_id
             LEFT JOIN rh_conversations c ON c.id = s.conversation_id")
            .bind(ids).bind(passages).fetch_all(&mut *self.tx).await.map_err(|source| Error::operation("summary detail", source))?;
        Ok(rows)
    }

    async fn summary_children(
        &mut self,
        ids: &[Uuid],
        expand: bool,
    ) -> Result<BTreeMap<Uuid, Vec<SummaryExcerpt>>, MemoryStoreError> {
        if !expand || ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<ChildRow> = sqlx::query_as("SELECT parent_id, to_json(kind) AS kind, content FROM rh_summaries WHERE parent_id = ANY($1) ORDER BY parent_id, summary_ordinal NULLS FIRST, earliest_at, id")
            .bind(ids).fetch_all(&mut *self.tx).await.map_err(|source| Error::operation("summary children", source))?;
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
        &mut self,
        ids: &[Uuid],
        query: &SearchQuery,
        expand: bool,
    ) -> Result<BTreeMap<Uuid, Vec<MemoryEntry>>, MemoryStoreError> {
        if !expand || ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<SourceRow> = sqlx::query_as(
            "SELECT wanted.id AS summary_id, picked.*
             FROM unnest($1::uuid[]) AS wanted(id)
             CROSS JOIN LATERAL (
                SELECT m.id::text AS id, c.session_key, m.agent, m.channel, m.role AS role_text,
                       m.content, m.tool_name, m.tool_args::text AS tool_args, m.tool_result,
                       m.metadata::text AS metadata, m.created_at, m.id AS message_id
                FROM (
                    SELECT message_id FROM rh_summary_message_ids(wanted.id)
                    UNION
                    SELECT ss.message_id FROM rh_summaries parent
                    JOIN rh_summaries child ON child.parent_id = parent.id
                    JOIN rh_summary_sources ss ON ss.summary_id = child.id
                    WHERE parent.id = wanted.id AND parent.pipeline_version < 7
                ) sources
                JOIN rh_messages m ON m.id = sources.message_id
                JOIN rh_conversations c ON c.id = m.conversation_id
                WHERE m.role <> 'tool' AND length(m.content) > 50
                AND ($2::text IS NULL OR m.agent = $2)
                AND ($3::timestamptz IS NULL OR m.created_at >= $3)
                AND ($4::timestamptz IS NULL OR m.created_at < $4)
                ORDER BY m.created_at, m.id LIMIT 200) picked
             ORDER BY wanted.id, picked.created_at, picked.message_id",
        )
        .bind(ids)
        .bind(&query.agent)
        .bind(query.since)
        .bind(query.before)
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("summary source messages", source))?;
        Ok(group_sources(rows))
    }

    async fn passage_sections(
        &mut self,
        ids: &[Uuid],
    ) -> Result<BTreeMap<Uuid, Vec<PassageSection>>, Error> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let sql = format!(
            "SELECT p.id AS passage_id, s.id, left(s.content, 500) AS content,
                    s.source_first, s.source_last, s.range_verified
             FROM rh_summary_passages p JOIN rh_summaries s ON {INTERSECTS}
             WHERE p.id = ANY($1)
             ORDER BY p.id, s.section_char_start, s.summary_ordinal, s.id"
        );
        let rows: Vec<SectionRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(ids)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|source| Error::operation("passage sections", source))?;
        let mut grouped: BTreeMap<Uuid, Vec<PassageSection>> = BTreeMap::new();
        for row in rows {
            let heading = row
                .content
                .lines()
                .find(|line| !line.trim().is_empty())
                .and_then(|line| line.strip_prefix("## "))
                .unwrap_or("Overview")
                .trim()
                .to_owned();
            grouped
                .entry(row.passage_id)
                .or_default()
                .push(PassageSection {
                    id: row.id.to_string(),
                    heading,
                    source_first: u32::try_from(row.source_first)?,
                    source_last: u32::try_from(row.source_last)?,
                    range_verified: row.range_verified,
                });
        }
        Ok(grouped)
    }

    async fn passage_sources(
        &mut self,
        ids: &[Uuid],
        query: &SearchQuery,
    ) -> Result<BTreeMap<Uuid, Vec<MemoryEntry>>, Error> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let sql = format!(
            "SELECT p.id AS summary_id, picked.*
             FROM rh_summary_passages p
             CROSS JOIN LATERAL (
                SELECT m.id::text AS id, c.session_key, m.agent, m.channel, m.role AS role_text,
                       left(m.content, 500) AS content, m.tool_name, NULL::text AS tool_args,
                       NULL::text AS tool_result, NULL::text AS metadata, m.created_at, m.id AS message_id
                FROM (
                    SELECT DISTINCT source.message_id
                    FROM (SELECT p.record_id) owner
                    LEFT JOIN rh_summaries s ON {INTERSECTS}
                    CROSS JOIN LATERAL rh_summary_message_ids(coalesce(s.id, owner.record_id)) source
                ) sources
                JOIN rh_messages m ON m.id = sources.message_id
                JOIN rh_conversations c ON c.id = m.conversation_id
                WHERE m.role <> 'tool' AND length(m.content) > 50
                AND ($2::text IS NULL OR m.agent = $2)
                AND ($3::timestamptz IS NULL OR m.created_at >= $3)
                AND ($4::timestamptz IS NULL OR m.created_at < $4)
                ORDER BY m.created_at, m.id LIMIT 4) picked
             WHERE p.id = ANY($1)
             ORDER BY p.id, picked.created_at, picked.message_id"
        );
        let rows = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(ids)
            .bind(&query.agent)
            .bind(query.since)
            .bind(query.before)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(|source| Error::operation("passage source messages", source))?;
        Ok(group_sources(rows))
    }
}

const INTERSECTS: &str = "s.parent_id = p.record_id AND s.kind = 'section'
    AND s.section_char_start < p.char_end AND p.char_start < s.section_char_end";

#[derive(sqlx::FromRow)]
struct SectionRow {
    passage_id: Uuid,
    id: Uuid,
    content: String,
    source_first: i32,
    source_last: i32,
    range_verified: bool,
}

fn group_sources(rows: Vec<SourceRow>) -> BTreeMap<Uuid, Vec<MemoryEntry>> {
    let mut grouped: BTreeMap<Uuid, Vec<MessageEntryRow>> = BTreeMap::new();
    for row in rows {
        grouped.entry(row.summary_id).or_default().push(row.entry);
    }
    grouped
        .into_iter()
        .map(|(id, rows)| (id, crate::read::readable_entries(rows)))
        .collect()
}
