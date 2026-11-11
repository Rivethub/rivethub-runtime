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
const TOOL_OUTPUTS: &str = "SELECT id, left(tool_result, 200000) AS content FROM rh_messages \
     WHERE id = ANY($1) AND tool_result IS NOT NULL AND length(tool_result) > 0";

const EXCERPT_CHARS: usize = 1500;

pub(crate) fn excerpt(output: &str, query: &str) -> String {
    let lowered = output.to_lowercase();
    let mut terms: Vec<String> = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| term.chars().count() >= 3)
        .collect();
    terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let found = terms
        .iter()
        .find_map(|term| lowered.find(term.as_str()))
        .filter(|_| lowered.len() == output.len());
    let total = output.chars().count();
    let start = found.map_or(0, |byte| {
        let at = output.get(..byte).map_or(0, |head| head.chars().count());
        at.saturating_sub(EXCERPT_CHARS / 3)
    });
    let end = (start + EXCERPT_CHARS).min(total);
    let mut text = String::new();
    if start > 0 {
        text.push('…');
    }
    text.extend(output.chars().skip(start).take(end - start));
    if end < total {
        text.push('…');
    }
    text
}
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
    pub(crate) async fn fill_contents(
        &self,
        hits: &mut Vec<MemorySearchHit>,
        query: &str,
    ) -> Result<(), Error> {
        let unchunked: Vec<MemorySearchHit> = hits
            .iter()
            .filter(|hit| hit.chunk.is_none())
            .cloned()
            .collect();
        let (mut messages, mut summaries, mut chunks, titles, outputs) = tokio::try_join!(
            self.contents(MESSAGE_CONTENTS, hit_uuids(hits, MemoryHitSource::Message)?),
            self.contents(SUMMARY_CONTENTS, hit_uuids(hits, MemoryHitSource::Summary)?),
            self.chunk_contents(hits),
            self.hit_titles(hits),
            self.contents(
                TOOL_OUTPUTS,
                hit_uuids(&unchunked, MemoryHitSource::Message)?
            ),
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
            let output = (hit.chunk.is_none() && hit.source == MemoryHitSource::Message)
                .then(|| outputs.get(&hit.id))
                .flatten();
            found
                .map(|content| {
                    hit.content = match output {
                        Some(output) => format!("{content}\n{}", excerpt(output, query)),
                        None => content,
                    };
                })
                .is_some()
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

#[cfg(test)]
mod tests {
    use super::EXCERPT_CHARS;
    use super::excerpt;

    #[test]
    fn an_excerpt_centres_on_the_most_specific_term_it_finds() {
        let output = format!(
            "{}RETRY_WIDGET_STAGES=1 retried{}",
            "x ".repeat(2000),
            " y".repeat(2000)
        );
        let text = excerpt(&output, "what env var: RETRY_WIDGET_STAGES?");
        assert!(text.contains("RETRY_WIDGET_STAGES=1 retried"), "{text}");
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert!(text.chars().count() <= EXCERPT_CHARS + 2);
    }

    #[test]
    fn an_excerpt_falls_back_to_the_head_and_keeps_short_output_whole() {
        let output = format!("head line\n{}", "z".repeat(3000));
        assert!(excerpt(&output, "nothing here matches").starts_with("head line"));
        assert_eq!(excerpt("ok", "anything"), "ok");
    }

    #[test]
    fn an_excerpt_never_splits_a_character() {
        let output = format!("{}needle{}", "é".repeat(1200), "ü".repeat(1200));
        assert!(excerpt(&output, "needle").contains("needle"));
    }
}
