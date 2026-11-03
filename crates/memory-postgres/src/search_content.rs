use std::collections::BTreeMap;

use kernel::memory::MemoryHitSource;
use kernel::memory::names_session;
use kernel::memory::summary_title;
use uuid::Uuid;

use crate::error::Error;
use crate::search::MemorySearchHit;
use crate::search::hit_uuids;

const MESSAGE_CONTENTS: &str = "SELECT id, content FROM rh_messages WHERE id = ANY($1)";
const SUMMARY_CONTENTS: &str = "SELECT id, content FROM rh_summaries WHERE id = ANY($1)";
const CHUNK_CONTENTS: &str = "SELECT c.message_id AS id, c.idx, c.content FROM rh_message_chunks c \
     JOIN unnest($1::uuid[], $2::int4[]) AS hit(message_id, idx) \
       ON hit.message_id = c.message_id AND hit.idx = c.idx";

const TITLES: &str = "SELECT c.id, c.title,
        CASE WHEN c.title IS NULL OR btrim(c.title) = '' THEN coalesce(
            (SELECT substring(convert_to(s.content, 'UTF8') from 1 for 2000) FROM rh_summaries s
              WHERE s.conversation_id = c.id ORDER BY s.latest_at DESC NULLS LAST, s.id LIMIT 1),
            (SELECT substring(convert_to(m.content, 'UTF8') from 1 for 2000) FROM rh_messages m
              WHERE m.conversation_id = c.id AND m.role = 'user' ORDER BY m.created_at, m.id LIMIT 1))
        END AS spoken
     FROM rh_conversations c WHERE c.id = ANY($1)";

#[derive(Debug, sqlx::FromRow)]
struct TitleRow {
    id: Uuid,
    title: Option<String>,
    spoken: Option<Vec<u8>>,
}

impl TitleRow {
    fn best(self) -> Option<(String, String)> {
        let title = self
            .title
            .filter(|title| names_session(title))
            .or_else(|| {
                self.spoken
                    .as_deref()
                    .and_then(|spoken| summary_title(&String::from_utf8_lossy(spoken)))
            })?;
        Some((self.id.to_string(), title))
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ContentRow {
    id: Uuid,
    content: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ChunkRow {
    id: Uuid,
    idx: i32,
    content: String,
}

impl crate::PostgresMemory {
    pub(crate) async fn fill_contents(&self, hits: &mut Vec<MemorySearchHit>) -> Result<(), Error> {
        let (mut messages, mut summaries, mut chunks, titles) = tokio::try_join!(
            self.contents(MESSAGE_CONTENTS, hit_uuids(hits, MemoryHitSource::Message)?),
            self.contents(SUMMARY_CONTENTS, hit_uuids(hits, MemoryHitSource::Summary)?),
            self.chunk_contents(hits),
            self.hit_titles(hits),
        )?;
        hits.retain_mut(|hit| {
            hit.title = hit
                .conversation_id
                .as_ref()
                .and_then(|conversation| titles.get(conversation))
                .cloned();
            if let Some(chunk) = &mut hit.chunk
                && let Some(content) = chunks.remove(&(hit.id.clone(), chunk.index))
            {
                chunk.content = content;
            }
            let found = match hit.source {
                MemoryHitSource::Message => messages.remove(&hit.id),
                MemoryHitSource::Summary => summaries.remove(&hit.id),
            };
            found.map(|content| hit.content = content).is_some()
        });
        Ok(())
    }

    async fn hit_titles(
        &self,
        hits: &[MemorySearchHit],
    ) -> Result<BTreeMap<String, String>, Error> {
        let mut conversations = hits
            .iter()
            .filter_map(|hit| hit.conversation_id.as_deref())
            .map(|id| {
                Uuid::parse_str(id).map_err(|error| {
                    Error::protocol(format!(
                        "search hit conversation {id} is not a uuid: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        conversations.sort_unstable();
        conversations.dedup();
        if conversations.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<TitleRow> = sqlx::query_as(TITLES)
            .bind(conversations)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("search titles", source))?;
        Ok(rows.into_iter().filter_map(TitleRow::best).collect())
    }

    async fn chunk_contents(
        &self,
        hits: &[MemorySearchHit],
    ) -> Result<BTreeMap<(String, u32), String>, Error> {
        let mut messages = Vec::new();
        let mut indexes = Vec::new();
        for hit in hits {
            if let Some(chunk) = &hit.chunk {
                messages.push(Uuid::parse_str(&hit.id).map_err(|error| {
                    Error::protocol(format!("search hit id {} is not a uuid: {error}", hit.id))
                })?);
                indexes.push(i32::try_from(chunk.index)?);
            }
        }
        if messages.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<ChunkRow> = sqlx::query_as(CHUNK_CONTENTS)
            .bind(messages)
            .bind(indexes)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("search contents", source))?;
        rows.into_iter()
            .map(|row| Ok(((row.id.to_string(), u32::try_from(row.idx)?), row.content)))
            .collect()
    }

    async fn contents(
        &self,
        statement: &'static str,
        ids: Vec<Uuid>,
    ) -> Result<BTreeMap<String, String>, Error> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let rows: Vec<ContentRow> = sqlx::query_as(statement)
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("search contents", source))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.id.to_string(), row.content))
            .collect())
    }
}
