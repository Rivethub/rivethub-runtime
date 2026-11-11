use std::collections::BTreeMap;

use kernel::memory::SessionActivity;
use kernel::memory::TRASH_MESSAGES_MAX;

use crate::PostgresMemory;
use crate::deletion::count;
use crate::error::Error;

struct ActivityRow {
    session_key: String,
    conversations: i64,
    messages: i64,
    user_messages: i64,
    assistant_messages: i64,
    tool_rows: i64,
    attachments: i64,
    tasked: bool,
    reply: Option<Vec<u8>>,
    reply_bytes: Option<i32>,
}

const REPLY_BYTES: i32 = 512;

fn reply_text(bytes: Vec<u8>, whole: Option<i32>) -> Result<String, Error> {
    let cut =
        whole.is_some_and(|whole| usize::try_from(whole).is_ok_and(|whole| whole > bytes.len()));
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(|source| Error::stored("trash reply", source))?
        }
    };
    if cut {
        text.push_str(" …");
    }
    Ok(text)
}

impl PostgresMemory {
    async fn activity_rows(&self, keys: &[String]) -> Result<Vec<ActivityRow>, Error> {
        let most = i64::try_from(TRASH_MESSAGES_MAX)?;
        sqlx::query_as!(
            ActivityRow,
            r#"WITH conv AS (
                   SELECT c.id, c.session_key, c.task_id,
                          (SELECT count(*) FROM (
                               SELECT 1 FROM rh_messages m WHERE m.conversation_id = c.id LIMIT $2::bigint + 1
                           ) probed) AS probed
                     FROM rh_conversations c
                    WHERE c.session_key = ANY($1)
               ), sessions AS (
                   SELECT session_key,
                          count(*) AS conversations,
                          sum(probed)::bigint AS messages,
                          bool_or(task_id IS NOT NULL) AS tasked
                     FROM conv
                    GROUP BY session_key
               ), talk AS MATERIALIZED (
                   SELECT conv.session_key,
                          count(*) FILTER (
                              WHERE m.role = 'user' AND m.content ~ '[^[:space:]]'
                          ) AS user_messages,
                          count(*) FILTER (WHERE m.role = 'assistant') AS assistant_messages,
                          count(*) FILTER (
                              WHERE m.role = 'tool' OR m.tool_name IS NOT NULL
                                 OR m.tool_args IS NOT NULL OR m.tool_result IS NOT NULL
                          ) AS tool_rows,
                          count(*) FILTER (
                              WHERE m.metadata ? 'attachments' OR m.metadata ? 'images'
                          ) AS attachments,
                          (array_agg(substring(convert_to(m.content, 'UTF8') from 1 for $3)
                                     ORDER BY m.created_at, m.id)
                              FILTER (WHERE m.role = 'assistant'))[1] AS reply,
                          (array_agg(octet_length(m.content) ORDER BY m.created_at, m.id)
                              FILTER (WHERE m.role = 'assistant'))[1] AS reply_bytes
                     FROM conv
                     JOIN sessions ON sessions.session_key = conv.session_key
                     JOIN rh_messages m ON m.conversation_id = conv.id
                    WHERE sessions.messages <= $2::bigint AND NOT sessions.tasked
                    GROUP BY conv.session_key
               )
               SELECT s.session_key AS "session_key!",
                      s.conversations AS "conversations!",
                      s.messages AS "messages!",
                      coalesce(t.user_messages, 0) AS "user_messages!",
                      coalesce(t.assistant_messages, 0) AS "assistant_messages!",
                      coalesce(t.tool_rows, 0) AS "tool_rows!",
                      coalesce(t.attachments, 0) AS "attachments!",
                      s.tasked AS "tasked!",
                      t.reply,
                      t.reply_bytes
                 FROM sessions s
                 LEFT JOIN talk t ON t.session_key = s.session_key"#,
            keys,
            most,
            REPLY_BYTES
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session activity", source))
    }

    async fn placed_keys(&self, keys: &[String]) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            r#"SELECT session_id AS "session_key!" FROM rh_session_placements
                WHERE session_id = ANY($1)
               UNION
               SELECT c.session_key FROM rh_conversation_placements p
                 JOIN rh_conversations c ON c.id = p.conversation_id
                WHERE c.session_key = ANY($1)"#,
            keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session placements", source))
    }

    async fn cited_keys(&self, keys: &[String]) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            r#"WITH conv AS (
                   SELECT id, session_key FROM rh_conversations WHERE session_key = ANY($1)
               ), sums AS (
                   SELECT s.id, conv.session_key
                     FROM rh_summaries s
                     JOIN conv ON conv.id = s.conversation_id
               )
               SELECT sums.session_key AS "session_key!" FROM sums
                WHERE EXISTS (SELECT 1 FROM rh_wiki_citations w WHERE w.summary_id = sums.id)
                   OR EXISTS (
                          SELECT 1 FROM rh_wiki_provenance p
                           WHERE p.source_kind = 'summary' AND p.source_id = sums.id
                      )
               UNION
               SELECT conv.session_key FROM conv
                WHERE EXISTS (SELECT 1 FROM rh_wiki_provenance p WHERE p.conversation_id = conv.id)
                   OR EXISTS (
                          SELECT 1 FROM rh_wiki_provenance p
                           WHERE p.source_kind = 'conversation' AND p.source_id = conv.id
                      )
               UNION
               SELECT conv.session_key
                 FROM rh_wiki_provenance p
                 JOIN rh_messages m ON m.id = p.source_id
                 JOIN conv ON conv.id = m.conversation_id
                WHERE p.source_kind = 'message'"#,
            keys
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session citations", source))
    }

    pub(crate) async fn activity(
        &self,
        keys: &[String],
    ) -> Result<BTreeMap<String, SessionActivity>, Error> {
        let mut activity: BTreeMap<String, SessionActivity> = keys
            .iter()
            .map(|key| (key.clone(), SessionActivity::default()))
            .collect();
        for row in self.activity_rows(keys).await? {
            activity.insert(
                row.session_key,
                SessionActivity {
                    conversations: count(row.conversations)?,
                    messages: count(row.messages)?,
                    user_messages: count(row.user_messages)?,
                    assistant_messages: count(row.assistant_messages)?,
                    reply: row
                        .reply
                        .map(|reply| reply_text(reply, row.reply_bytes))
                        .transpose()?,
                    tool_rows: count(row.tool_rows)?,
                    attachments: count(row.attachments)?,
                    tasked: row.tasked,
                    placed: false,
                    cited: false,
                },
            );
        }
        let open: Vec<String> = activity
            .iter()
            .filter(|(_, entry)| !entry.kept())
            .map(|(key, _)| key.clone())
            .collect();
        let (placed, cited) = tokio::try_join!(self.placed_keys(&open), self.cited_keys(&open))?;
        for (key, entry) in &mut activity {
            entry.placed = placed.contains(key);
            entry.cited = cited.contains(key);
        }
        Ok(activity)
    }
}
