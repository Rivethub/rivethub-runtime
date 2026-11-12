use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::filing::Filable;
use kernel::memory::ConversationHead;
use kernel::memory::Provenance;
use kernel::wiki::WikiCitation;
use sqlx::types::Json;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::IMPORTED_SESSION_PREFIX;
use wire::session_id::SessionId;

use crate::Error;
use crate::PostgresMemory;

const CITATIONS_SQL: &str = "SELECT w.summary_id, w.kind, w.note, w.cited_at,
        c.id AS conversation_id, c.session_key, c.agent, c.channel, c.title, p.topic_id AS filed_in,
        CASE WHEN starts_with(c.session_key, $2) THEN coalesce(c.settings, '{}'::jsonb) END AS import_settings
   FROM rh_wiki_citations w
   LEFT JOIN rh_summaries s ON s.id = w.summary_id
   LEFT JOIN rh_conversations c ON c.id = s.conversation_id
   LEFT JOIN rh_conversation_placements p ON p.conversation_id = c.id
  WHERE w.topic_slug = $1
  ORDER BY w.cited_at, w.summary_id";

const IMPORTED_SQL: &str = "SELECT DISTINCT w.topic_slug, coalesce(c.settings, '{}'::jsonb)
   FROM rh_wiki_citations w
   JOIN rh_summaries s ON s.id = w.summary_id
   JOIN rh_conversations c ON c.id = s.conversation_id
  WHERE w.topic_slug = ANY($1) AND starts_with(c.session_key, $2)";

const PLACED_SQL: &str =
    "SELECT session_id, topic_id FROM rh_session_placements WHERE session_id = ANY($1)";

#[derive(Debug, sqlx::FromRow)]
struct CitationRow {
    summary_id: Uuid,
    kind: Option<String>,
    note: Option<String>,
    cited_at: DateTime<Utc>,
    conversation_id: Option<Uuid>,
    session_key: Option<String>,
    agent: Option<String>,
    channel: Option<String>,
    title: Option<String>,
    filed_in: Option<Uuid>,
    import_settings: Option<Json<JsonObject>>,
}

impl CitationRow {
    fn session(&self) -> Option<SessionId> {
        let head = ConversationHead {
            id: self.conversation_id?,
            session_key: self.session_key.clone()?,
            agent: self.agent.clone()?,
            channel: self.channel.clone()?,
            title: self.title.clone(),
        };
        Filable::of(&head).target(head.id)?.session().cloned()
    }
}

impl PostgresMemory {
    pub(crate) async fn read_citations(&self, slug: &str) -> Result<Vec<WikiCitation>, Error> {
        let rows: Vec<CitationRow> = sqlx::query_as(CITATIONS_SQL)
            .bind(slug)
            .bind(IMPORTED_SESSION_PREFIX)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("wiki citations", source))?;
        let sessions: Vec<Option<SessionId>> = rows.iter().map(CitationRow::session).collect();
        let keys: Vec<&str> = sessions.iter().flatten().map(SessionId::as_str).collect();
        let placed: BTreeMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(PLACED_SQL)
            .bind(&keys)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("wiki citation placements", source))?
            .into_iter()
            .collect();
        Ok(rows
            .into_iter()
            .zip(sessions)
            .map(|(row, session)| {
                let topic = match &session {
                    Some(session) => placed.get(session.as_str()).copied(),
                    None => row.filed_in,
                };
                WikiCitation {
                    summary_id: row.summary_id.to_string(),
                    date: Some(row.cited_at),
                    kind: row.kind,
                    note: row.note,
                    conversation_id: row.conversation_id.map(|id| id.to_string()),
                    session_id: session,
                    title: row.title,
                    channel: row.channel,
                    topic_id: topic.map(|id| id.to_string()),
                    imported: row
                        .import_settings
                        .map(|settings| Provenance::from_settings(&settings.0)),
                }
            })
            .collect())
    }

    pub(crate) async fn wiki_imported_authors(
        &self,
        slugs: &[String],
    ) -> Result<BTreeMap<String, Vec<Provenance>>, Error> {
        let mut found: BTreeMap<String, Vec<Provenance>> = BTreeMap::new();
        if slugs.is_empty() {
            return Ok(found);
        }
        let rows: Vec<(String, Json<JsonObject>)> = sqlx::query_as(IMPORTED_SQL)
            .bind(slugs)
            .bind(IMPORTED_SESSION_PREFIX)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("wiki imported authors", source))?;
        for (slug, settings) in rows {
            found
                .entry(slug)
                .or_default()
                .push(Provenance::from_settings(&settings.0));
        }
        Ok(found)
    }
}
