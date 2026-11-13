use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::str::FromStr;

use kernel::filing::Filable;
use kernel::memory::ConversationHead;
use kernel::memory::Provenance;
use kernel::wiki::WikiCitation;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::IMPORTED_SESSION_PREFIX;
use wire::session_id::SessionId;

use crate::error::SqliteError;
use crate::time;

const CITATIONS_SQL: &str = "SELECT w.summary_id, w.kind, w.note, w.cited_at, \
        c.id, c.session_key, c.agent, c.channel, c.title, p.topic_id, \
        CASE WHEN substr(c.session_key, 1, length(?2)) = ?2 THEN c.settings END \
   FROM ros_wiki_citations w \
   LEFT JOIN ros_summaries s ON s.id = w.summary_id \
   LEFT JOIN ros_conversations c ON c.id = s.conversation_id \
   LEFT JOIN rh_conversation_placements p ON p.conversation_id = c.id \
  WHERE w.topic_slug = ?1 \
  ORDER BY w.cited_at, w.summary_id";

const IMPORTED_SQL: &str = "SELECT DISTINCT w.topic_slug, c.settings \
   FROM ros_wiki_citations w \
   JOIN ros_summaries s ON s.id = w.summary_id \
   JOIN ros_conversations c ON c.id = s.conversation_id \
  WHERE w.topic_slug IN (SELECT value FROM json_each(?1)) \
    AND substr(c.session_key, 1, length(?2)) = ?2 \
  ORDER BY w.topic_slug, c.session_key";

const PLACED_SQL: &str = "SELECT topic_id FROM rh_session_placements WHERE session_id = ?1";

struct Row {
    summary_id: String,
    kind: Option<String>,
    note: Option<String>,
    cited_at: String,
    conversation_id: Option<Uuid>,
    session_key: Option<String>,
    agent: Option<String>,
    channel: Option<String>,
    title: Option<String>,
    filed_in: Option<String>,
    import_settings: Option<String>,
}

impl Row {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            summary_id: row.get(0)?,
            kind: row.get(1)?,
            note: row.get(2)?,
            cited_at: row.get(3)?,
            conversation_id: row
                .get::<_, Option<String>>(4)?
                .map(|id| Uuid::from_str(&id))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            session_key: row.get(5)?,
            agent: row.get(6)?,
            channel: row.get(7)?,
            title: row.get(8)?,
            filed_in: row.get(9)?,
            import_settings: row.get(10)?,
        })
    }

    fn session(&self) -> Option<SessionId> {
        let id = self.conversation_id?;
        let head = ConversationHead {
            id,
            session_key: self.session_key.clone()?,
            agent: self.agent.clone()?,
            channel: self.channel.clone()?,
            title: self.title.clone(),
        };
        Filable::of(&head).target(id)?.session().cloned()
    }
}

pub(crate) fn citations(conn: &Connection, slug: &str) -> Result<Vec<WikiCitation>, SqliteError> {
    let mut statement = conn
        .prepare(CITATIONS_SQL)
        .map_err(SqliteError::query("wiki citations"))?;
    let rows = statement
        .query_map(params![slug, IMPORTED_SESSION_PREFIX], Row::read)
        .map_err(SqliteError::query("wiki citations"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki citations"))?;
    let sessions: Vec<Option<SessionId>> = rows.iter().map(Row::session).collect();
    let distinct: BTreeSet<&str> = sessions.iter().flatten().map(SessionId::as_str).collect();
    let mut placed: BTreeMap<&str, String> = BTreeMap::new();
    for session in distinct {
        let topic = conn
            .query_row(PLACED_SQL, params![session], |row| row.get(0))
            .optional()
            .map_err(SqliteError::query("wiki citation placements"))?;
        if let Some(topic) = topic {
            placed.insert(session, topic);
        }
    }
    rows.into_iter()
        .zip(sessions.iter())
        .map(|(row, session)| {
            let topic = match session {
                Some(session) => placed.get(session.as_str()).cloned(),
                None => row.filed_in,
            };
            Ok(WikiCitation {
                summary_id: row.summary_id,
                date: Some(time::stored(&row.cited_at, "cited_at")?),
                kind: row.kind,
                note: row.note,
                conversation_id: row.conversation_id.map(|id| id.to_string()),
                session_id: session.clone(),
                title: row.title,
                channel: row.channel,
                topic_id: topic,
                imported: row.import_settings.as_deref().map(provenance).transpose()?,
            })
        })
        .collect()
}

fn provenance(settings: &str) -> Result<Provenance, SqliteError> {
    let settings: JsonObject =
        serde_json::from_str(settings).map_err(|source| SqliteError::StoredJson {
            column: "settings",
            source,
        })?;
    Ok(Provenance::from_settings(&settings))
}

pub(crate) fn imported_authors(
    conn: &Connection,
    slugs: &[String],
) -> Result<BTreeMap<String, Vec<Provenance>>, SqliteError> {
    let mut found: BTreeMap<String, Vec<Provenance>> = BTreeMap::new();
    if slugs.is_empty() {
        return Ok(found);
    }
    let wanted = serde_json::to_string(slugs).map_err(|source| SqliteError::Encode {
        column: "wiki slugs",
        source,
    })?;
    let mut statement = conn
        .prepare(IMPORTED_SQL)
        .map_err(SqliteError::query("wiki imported authors"))?;
    let rows = statement
        .query_map(params![wanted, IMPORTED_SESSION_PREFIX], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(SqliteError::query("wiki imported authors"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki imported authors"))?;
    for (slug, settings) in rows {
        found.entry(slug).or_default().push(provenance(&settings)?);
    }
    Ok(found)
}
