use std::str::FromStr as _;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::filing::stored_keys;
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
use strum::IntoEnumIterator as _;
use uuid::Uuid;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::PostgresMemory;
use crate::error::Error;

pub(crate) const BARE_UUID: &str = r"^([0-9a-f]{32}|[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}|\{[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}\})$";

const SESSIONS: &str = r"
WITH conv AS (
    SELECT c.id, c.created_at, c.updated_at, c.active, c.title,
           CASE WHEN c.channel = $11 AND c.session_key ~* $12 THEN $11 || ':' || c.session_key
                ELSE c.session_key END AS session_key
      FROM rh_conversations c
     WHERE c.task_id IS NULL
       AND ($10::text[] IS NULL OR c.session_key = ANY($10))
), sess AS (
    SELECT c.session_key,
           min(least(c.created_at, (SELECT min(m.created_at) FROM rh_messages m
                                      WHERE m.conversation_id = c.id))) AS started_at,
           max(c.updated_at) AS updated_at,
           bool_or(coalesce(c.active, false)) AS active,
           (array_agg(c.title ORDER BY c.updated_at DESC, c.id)
               FILTER (WHERE c.title ~ '[^[:space:]]'))[1] AS title,
           array_agg(c.id) AS conversations
      FROM conv c
     WHERE split_part(c.session_key, ':', 1) = ANY($1)
       AND strpos(c.session_key, ':') > 0
       AND substr(c.session_key, strpos(c.session_key, ':') + 1) <> ''
       AND ($2::text IS NULL OR c.session_key = $2)
       AND c.session_key <> ALL($3)
     GROUP BY c.session_key
), page AS (
    SELECT s.session_key, s.conversations, s.started_at, s.updated_at, s.active, s.title,
           substr(s.session_key, strpos(s.session_key, ':') + 1) AS native,
           k.session_key IS NOT NULL AS kept,
           tr.session_key IS NOT NULL AS trashed,
           p.topic_id
      FROM sess s
      LEFT JOIN rh_session_keeps k ON k.session_key = s.session_key
      LEFT JOIN rh_session_trash tr ON tr.session_key = s.session_key
      LEFT JOIN rh_session_placements p ON p.session_id = s.session_key
     WHERE CASE $4::text
               WHEN 'all' THEN tr.session_key IS NULL
               WHEN 'unfiled' THEN tr.session_key IS NULL AND p.topic_id IS NULL
               WHEN 'kept' THEN tr.session_key IS NULL AND k.session_key IS NOT NULL
               WHEN 'trash' THEN tr.session_key IS NOT NULL
               ELSE true
           END
       AND ($5::uuid IS NULL OR p.topic_id = $5
            OR p.topic_id IN (SELECT t.id FROM rh_topics t WHERE t.parent_id = $5))
       AND ($6::text IS NULL
            OR strpos(lower(coalesce(s.title, substr(s.session_key, strpos(s.session_key, ':') + 1))),
                      lower($6)) > 0
            OR strpos(lower(s.session_key), lower($6)) > 0)
       AND ($7::timestamptz IS NULL OR (s.started_at, s.session_key) < ($7, $8::text))
     ORDER BY s.started_at DESC, s.session_key DESC
     LIMIT $9
), flagged AS MATERIALIZED (
    SELECT coalesce(
               CASE WHEN f.payload->>'conversationId' ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
                    THEN (f.payload->>'conversationId')::uuid END,
               (SELECT m.conversation_id FROM rh_messages m
                 WHERE m.id = CASE WHEN target.message ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
                                   THEN target.message::uuid END)) AS conversation_id
      FROM rh_flagged_jobs f
     CROSS JOIN LATERAL (
           SELECT coalesce(f.payload->>'messageId',
                           CASE WHEN f.payload->>'targetTable' = 'ros_messages'
                                THEN f.payload->>'targetId' END) AS message) AS target
     WHERE f.dismissed_at IS NULL
)
SELECT g.session_key, g.title, g.started_at, g.updated_at, g.active, g.kept, g.trashed, g.native,
       g.topic_id,
       (SELECT t.name FROM rh_topics t WHERE t.id = g.topic_id) AS topic_name,
       (SELECT pt.name FROM rh_topics t JOIN rh_topics pt ON pt.id = t.parent_id
         WHERE t.id = g.topic_id) AS parent_name,
       (SELECT count(*) FROM rh_messages m
         WHERE m.conversation_id = ANY(g.conversations)) AS message_count,
       EXISTS (SELECT 1 FROM rh_summaries s
                WHERE s.conversation_id = ANY(g.conversations)) AS summarized,
       EXISTS (SELECT 1 FROM flagged f
                WHERE f.conversation_id = ANY(g.conversations)) AS flagged,
       (SELECT s.content FROM rh_summaries s
         WHERE s.conversation_id = ANY(g.conversations)
         ORDER BY s.depth DESC, s.created_at DESC, s.id LIMIT 1) AS summary,
       (SELECT c.settings->>'cwd' FROM rh_conversations c
         WHERE c.id = ANY(g.conversations) AND jsonb_typeof(c.settings->'cwd') = 'string'
         ORDER BY c.updated_at DESC, c.id LIMIT 1) AS cwd,
       (SELECT left(m.content, 600) FROM rh_messages m
         WHERE m.conversation_id = ANY(g.conversations) AND m.role = 'user' AND m.content ~ '[^[:space:]]'
         ORDER BY m.created_at, m.id LIMIT 1) AS first_user
  FROM page g
 ORDER BY g.started_at DESC, g.session_key DESC";

#[derive(sqlx::FromRow)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one column per SQL flag of a session row"
)]
struct Row {
    session_key: String,
    title: Option<String>,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    active: bool,
    kept: bool,
    trashed: bool,
    native: String,
    topic_id: Option<Uuid>,
    topic_name: Option<String>,
    parent_name: Option<String>,
    message_count: i64,
    summarized: bool,
    flagged: bool,
    summary: Option<String>,
    cwd: Option<String>,
    first_user: Option<String>,
}

impl Row {
    fn record(self) -> Result<SessionRecord, Error> {
        let id = SessionId::from_str(&self.session_key)
            .map_err(|source| Error::stored("session key", source))?;
        let topic = match (self.topic_id, self.topic_name) {
            (Some(topic), Some(name)) => Some(SessionTopic {
                id: TopicId::from_uuid(topic),
                path: self.parent_name.into_iter().chain([name]).collect(),
            }),
            _ => None,
        };
        Ok(SessionRecord {
            id,
            title: session_title(
                self.title.as_deref(),
                self.summary.as_deref(),
                self.first_user.as_deref(),
                &self.native,
            ),
            started_at: self.started_at,
            ended_at: (!self.active).then_some(self.updated_at),
            message_count: u64::try_from(self.message_count)?,
            topic,
            kept: self.kept,
            trashed: self.trashed,
            memory: memory_state(self.active, self.flagged, self.summarized),
            gist: self.summary.as_deref().and_then(snippet),
            cwd: self.cwd,
        })
    }
}

fn harnesses(only: Option<HarnessId>) -> Vec<String> {
    match only {
        Some(harness) => vec![harness.to_string()],
        None => HarnessId::iter()
            .map(|harness| harness.to_string())
            .collect(),
    }
}

struct Select<'a> {
    key: Option<&'a str>,
    view: Option<SessionView>,
    harness: Option<HarnessId>,
    exclude: &'a [String],
    topic: Option<TopicId>,
    text: Option<&'a str>,
    after: Option<&'a SessionCursor>,
    limit: i64,
}

impl<'a> Select<'a> {
    fn page(query: &'a SessionQuery) -> Self {
        Self {
            key: None,
            view: Some(query.view),
            harness: query.harness,
            exclude: &query.exclude,
            topic: query.topic,
            text: query.text.as_deref(),
            after: query.after.as_ref(),
            limit: i64::from(query.page_size()) + 1,
        }
    }

    fn one(session: &'a SessionId) -> Self {
        Self {
            key: Some(session.as_str()),
            view: None,
            harness: None,
            exclude: &[],
            topic: None,
            text: None,
            after: None,
            limit: 1,
        }
    }
}

impl PostgresMemory {
    async fn sessions(&self, select: Select<'_>) -> Result<Vec<SessionRecord>, Error> {
        let rows: Vec<Row> = sqlx::query_as(SESSIONS)
            .bind(harnesses(select.harness))
            .bind(select.key)
            .bind(select.exclude)
            .bind(select.view.map(|view| view.to_string()))
            .bind(select.topic.map(TopicId::uuid))
            .bind(select.text)
            .bind(select.after.map(|cursor| cursor.started_at))
            .bind(select.after.map(|cursor| cursor.key.as_str()))
            .bind(select.limit)
            .bind(select.key.map(stored_keys))
            .bind(HarnessId::ClaudeCode.to_string())
            .bind(BARE_UUID)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("session list", source))?;
        rows.into_iter().map(Row::record).collect()
    }

    async fn session_page(&self, query: &SessionQuery) -> Result<SessionPage, Error> {
        let size = usize::try_from(query.page_size())?;
        let mut items = self.sessions(Select::page(query)).await?;
        let more = items.len() > size;
        items.truncate(size);
        let next = more
            .then(|| items.last())
            .flatten()
            .map(|last| SessionCursor {
                started_at: last.started_at,
                key: last.id.to_string(),
            });
        Ok(SessionPage { items, next })
    }
}

#[async_trait]
impl SessionCatalog for PostgresMemory {
    async fn list(&self, query: &SessionQuery) -> Result<SessionPage, MemoryStoreError> {
        self.session_page(query).await.map_err(Into::into)
    }

    async fn get(&self, session: &SessionId) -> Result<Option<SessionRecord>, MemoryStoreError> {
        let mut found = self.sessions(Select::one(session)).await?;
        Ok(found.pop())
    }
}
