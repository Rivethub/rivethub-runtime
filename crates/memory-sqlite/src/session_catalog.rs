use std::str::FromStr as _;

use async_trait::async_trait;
use kernel::filing::session_key;
use kernel::filing::stored_keys;
use kernel::memory::ImportOrigin;
use kernel::memory::ImportReceipt;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionCursor;
use kernel::memory::SessionPage;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use kernel::memory::SessionTopic;
use kernel::memory::SessionView;
use kernel::memory::memory_state;
use kernel::memory::session_title;
use kernel::memory::snippet;
use kernel::topics::TopicId;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::named_params;
use strum::IntoEnumIterator as _;
use wire::JsonObject;
use wire::capture::SessionOrigin;
use wire::harness::HarnessId;
use wire::session_id::SessionKey;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const SESSIONS: &str = "
WITH conv AS (
    SELECT c.id, c.created_at, c.updated_at, c.active,
           rh_session_key(c.channel, c.session_key) AS skey
      FROM ros_conversations c
     WHERE c.task_id IS NULL
       AND (:keys IS NULL OR c.session_key IN (SELECT value FROM json_each(:keys)))
), sess AS (
    SELECT c.skey AS key,
           min(julianday(c.created_at)) AS started,
           max(julianday(c.updated_at)) AS updated,
           max(c.active) AS active,
           json_group_array(c.id) AS conversations
      FROM conv c
     WHERE (substr(c.skey, 1, instr(c.skey, ':') - 1) IN (SELECT value FROM json_each(:harnesses))
            OR (:imports AND length(c.skey) = 71 AND substr(c.skey, 1, 7) = 'import:'
                AND NOT substr(c.skey, 8) GLOB '*[^0-9a-f]*'))
       AND (:sessions IS NULL OR c.skey IN (SELECT value FROM json_each(:sessions)))
       AND c.skey NOT IN (SELECT value FROM json_each(:exclude))
     GROUP BY c.skey
), titled AS (
    SELECT s.*,
           (SELECT t.title FROM ros_conversations t
             WHERE t.id IN (SELECT value FROM json_each(s.conversations))
               AND trim(coalesce(t.title, '')) <> ''
             ORDER BY julianday(t.updated_at) DESC, t.id LIMIT 1) AS title
      FROM sess s
), page AS (
    SELECT s.key, s.conversations, s.started, s.updated, s.active, s.title,
           substr(s.key, instr(s.key, ':') + 1) AS native,
           k.session_key IS NOT NULL AS kept,
           tr.session_key IS NOT NULL AS trashed,
           p.topic_id AS topic_id
      FROM titled s
      LEFT JOIN rh_session_keeps k ON k.session_key = s.key
      LEFT JOIN rh_session_trash tr ON tr.session_key = s.key
      LEFT JOIN rh_session_placements p ON p.session_id = s.key
     WHERE VIEW_CONDITION
       AND (:topic IS NULL OR p.topic_id = :topic
            OR p.topic_id IN (SELECT id FROM rh_topics WHERE parent_id = :topic))
       AND (:text IS NULL
            OR instr(lower(coalesce(s.title, substr(s.key, instr(s.key, ':') + 1))), lower(:text)) > 0
            OR instr(lower(s.key), lower(:text)) > 0)
       AND (:after_key IS NULL OR s.started < :after_started
            OR (s.started = :after_started AND s.key < :after_key))
     ORDER BY s.started DESC, s.key DESC
     LIMIT :limit
)
SELECT g.key,
       g.title,
       strftime('%Y-%m-%dT%H:%M:%fZ', g.started),
       strftime('%Y-%m-%dT%H:%M:%fZ', g.updated),
       g.active,
       (SELECT count(*) FROM ros_messages m
         WHERE m.conversation_id IN (SELECT value FROM json_each(g.conversations))),
       g.kept,
       g.trashed,
       g.topic_id,
       (SELECT t.name FROM rh_topics t WHERE t.id = g.topic_id),
       (SELECT pt.name FROM rh_topics t JOIN rh_topics pt ON pt.id = t.parent_id WHERE t.id = g.topic_id),
       EXISTS (SELECT 1 FROM ros_summaries s
                WHERE s.conversation_id IN (SELECT value FROM json_each(g.conversations))),
       EXISTS (SELECT 1 FROM ros_jobs j
                 LEFT JOIN ros_messages m
                        ON m.id = coalesce(
                               CASE WHEN json_valid(j.payload) THEN json_extract(j.payload, '$.messageId') END,
                               CASE WHEN json_valid(j.payload) AND json_extract(j.payload, '$.targetTable') = 'ros_messages'
                                    THEN json_extract(j.payload, '$.targetId') END)
                 JOIN ros_conversations c
                   ON c.id = coalesce(
                          CASE WHEN json_valid(j.payload) THEN json_extract(j.payload, '$.conversationId') END,
                          m.conversation_id)
                WHERE j.state = 'dead' AND c.id IN (SELECT value FROM json_each(g.conversations))),
       (SELECT s.content FROM ros_summaries s
         WHERE s.conversation_id IN (SELECT value FROM json_each(g.conversations))
         ORDER BY s.depth DESC, julianday(s.created_at) DESC LIMIT 1),
       (SELECT json_extract(c.settings, '$.cwd') FROM ros_conversations c
         WHERE c.id IN (SELECT value FROM json_each(g.conversations)) AND json_valid(c.settings)
           AND json_type(c.settings, '$.cwd') = 'text'
         ORDER BY julianday(c.updated_at) DESC LIMIT 1),
       (SELECT CASE WHEN json_valid(c.settings) THEN c.settings END FROM ros_conversations c
         WHERE c.id IN (SELECT value FROM json_each(g.conversations)) AND g.key LIKE 'import:%'
         ORDER BY julianday(c.updated_at) DESC LIMIT 1),
       (SELECT substr(m.content, 1, 600) FROM ros_messages m
         WHERE m.conversation_id IN (SELECT value FROM json_each(g.conversations)) AND m.role = 'user' AND trim(m.content) <> ''
         ORDER BY julianday(m.created_at), m.rowid LIMIT 1),
       g.native
  FROM page g
 ORDER BY g.started DESC, g.key DESC";

struct Row {
    key: String,
    title: Option<String>,
    started: String,
    updated: String,
    active: i64,
    messages: i64,
    kept: i64,
    trashed: i64,
    topic: Option<String>,
    topic_name: Option<String>,
    parent_name: Option<String>,
    summarized: i64,
    flagged: i64,
    summary: Option<String>,
    cwd: Option<String>,
    import_settings: Option<String>,
    first_user: Option<String>,
    native: String,
}

fn import_origin(settings: Option<&str>) -> Result<ImportOrigin, SqliteError> {
    let settings: Option<JsonObject> = settings
        .map(|text| {
            serde_json::from_str(text).map_err(|source| SqliteError::StoredJson {
                column: "settings",
                source,
            })
        })
        .transpose()?;
    let receipt = settings.as_ref().and_then(ImportReceipt::from_settings);
    let origin = settings.and_then(|settings| SessionOrigin::of(&settings));
    Ok(ImportOrigin {
        receipt,
        author: origin
            .as_ref()
            .and_then(SessionOrigin::known_author)
            .map(str::to_owned),
        created_at: origin.as_ref().and_then(|origin| origin.created_at),
        updated_at: origin.as_ref().and_then(|origin| origin.updated_at),
    })
}

impl Row {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            key: row.get(0)?,
            title: row.get(1)?,
            started: row.get(2)?,
            updated: row.get(3)?,
            active: row.get(4)?,
            messages: row.get(5)?,
            kept: row.get(6)?,
            trashed: row.get(7)?,
            topic: row.get(8)?,
            topic_name: row.get(9)?,
            parent_name: row.get(10)?,
            summarized: row.get(11)?,
            flagged: row.get(12)?,
            summary: row.get(13)?,
            cwd: row.get(14)?,
            import_settings: row.get(15)?,
            first_user: row.get(16)?,
            native: row.get(17)?,
        })
    }

    fn record(self) -> Result<SessionRecord<SessionKey>, SqliteError> {
        let id = SessionKey::from_str(&self.key).map_err(|_| SqliteError::StoredSession {
            value: self.key.clone(),
        })?;
        let topic = match (self.topic, self.topic_name) {
            (Some(raw), Some(name)) => {
                let id = TopicId::from_str(&raw)
                    .map_err(|_| SqliteError::StoredTopic { value: raw.clone() })?;
                let path = self.parent_name.into_iter().chain([name]).collect();
                Some(SessionTopic { id, path })
            }
            _ => None,
        };
        let updated = time::stored(&self.updated, "ros_conversations.updated_at")?;
        let origin = match id {
            SessionKey::Import(_) => Some(import_origin(self.import_settings.as_deref())?),
            SessionKey::Harness(_) => None,
        };
        Ok(SessionRecord {
            owner: None,
            id,
            title: session_title(
                self.title.as_deref(),
                self.summary.as_deref(),
                self.first_user.as_deref(),
                &self.native,
            ),
            started_at: time::stored(&self.started, "ros_conversations.created_at")?,
            ended_at: (self.active == 0).then_some(updated),
            message_count: u64::try_from(self.messages).map_err(SqliteError::Count)?,
            topic,
            kept: self.kept != 0,
            trashed: self.trashed != 0,
            memory: memory_state(self.active != 0, self.flagged != 0, self.summarized != 0),
            gist: self.summary.as_deref().and_then(snippet),
            cwd: self.cwd,
            origin,
        })
    }
}

fn view_condition(view: Option<SessionView>) -> &'static str {
    match view {
        None => "1",
        Some(SessionView::All) => "tr.session_key IS NULL",
        Some(SessionView::Unfiled) => "tr.session_key IS NULL AND p.topic_id IS NULL",
        Some(SessionView::Kept) => "tr.session_key IS NULL AND k.session_key IS NOT NULL",
        Some(SessionView::Trash) => "tr.session_key IS NOT NULL",
    }
}

pub(crate) fn harnesses(only: Option<HarnessId>) -> Result<String, SqliteError> {
    let names: Vec<String> = match only {
        Some(harness) => vec![harness.to_string()],
        None => HarnessId::iter()
            .map(|harness| harness.to_string())
            .collect(),
    };
    serde_json::to_string(&names).map_err(|source| SqliteError::Encode {
        column: "harness names",
        source,
    })
}

fn keys_json(keys: &[String]) -> Result<String, SqliteError> {
    serde_json::to_string(keys).map_err(|source| SqliteError::Encode {
        column: "session keys",
        source,
    })
}

struct Scope<'a> {
    view: Option<SessionView>,
    imports: bool,
    harnesses: &'a str,
    exclude: &'a str,
    sessions: Option<&'a str>,
    keys: Option<&'a str>,
}

pub(crate) fn register(conn: &Connection) -> Result<(), SqliteError> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("rh_session_key", 2, flags, |ctx| {
        let channel = ctx.get::<Option<String>>(0)?;
        let key = ctx.get::<Option<String>>(1)?;
        Ok(key.map(|key| session_key(channel.as_deref(), &key)))
    })
    .map_err(SqliteError::query("register session key"))
}

fn select(
    conn: &Connection,
    scope: &Scope<'_>,
    query: Option<&SessionQuery>,
    limit: i64,
) -> Result<Vec<SessionRecord<SessionKey>>, SqliteError> {
    let sql = SESSIONS.replace("VIEW_CONDITION", view_condition(scope.view));
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("session list"))?;
    let after = query.and_then(|query| query.after.as_ref());
    let after_started = after
        .map(|cursor| {
            conn.query_row(
                "SELECT julianday(?1)",
                [time::stamp(cursor.started_at)],
                |row| row.get::<_, f64>(0),
            )
        })
        .transpose()
        .map_err(SqliteError::query("session cursor"))?;
    let rows = statement
        .query_map(
            named_params! {
                ":harnesses": scope.harnesses,
                ":imports": scope.imports,
                ":sessions": scope.sessions,
                ":keys": scope.keys,
                ":topic": query.and_then(|query| query.topic).map(|topic| topic.to_string()),
                ":text": query.and_then(|query| query.text.as_deref()),
                ":after_key": after.map(|cursor| cursor.key.as_str()),
                ":after_started": after_started,
                ":exclude": scope.exclude,
                ":limit": limit,
            },
            Row::read,
        )
        .map_err(SqliteError::query("session list"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("session list"))?;
    rows.into_iter().map(Row::record).collect()
}

fn page(conn: &Connection, query: &SessionQuery) -> Result<SessionPage, SqliteError> {
    let size = query.page_size();
    let wanted = i64::from(size) + 1;
    let scope = Scope {
        view: Some(query.view),
        imports: query.harness.is_none(),
        harnesses: &harnesses(query.harness)?,
        exclude: &keys_json(&query.exclude)?,
        sessions: None,
        keys: None,
    };
    let mut items = select(conn, &scope, Some(query), wanted)?;
    let more = items.len() > usize::try_from(size).map_err(SqliteError::Count)?;
    items.truncate(usize::try_from(size).map_err(SqliteError::Count)?);
    let next = if more {
        items.last().map(|last| SessionCursor {
            started_at: last.started_at,
            key: last.id.to_string(),
        })
    } else {
        None
    };
    Ok(SessionPage { items, next })
}

#[async_trait]
impl SessionCatalog for SqliteStore {
    async fn list(&self, query: &SessionQuery) -> Result<SessionPage, MemoryStoreError> {
        let query = query.clone();
        self.with_conn(move |conn| page(conn, &query))
            .await
            .map_err(Into::into)
    }

    async fn entries(
        &self,
        keys: &[SessionKey],
    ) -> Result<Vec<SessionRecord<SessionKey>>, MemoryStoreError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<String> = keys.iter().map(ToString::to_string).collect();
        let stored: Vec<String> = keys
            .iter()
            .flat_map(|key| stored_keys(key.as_str()))
            .collect();
        let limit = i64::try_from(wanted.len()).map_err(SqliteError::Count)?;
        let wanted = keys_json(&wanted)?;
        let stored = keys_json(&stored)?;
        self.with_conn(move |conn| {
            select(
                conn,
                &Scope {
                    view: None,
                    imports: true,
                    harnesses: &harnesses(None)?,
                    exclude: "[]",
                    sessions: Some(&wanted),
                    keys: Some(&stored),
                },
                None,
                limit,
            )
        })
        .await
        .map_err(Into::into)
    }
}
