use kernel::memory::MemoryStatsResponse;
use kernel::memory::RecentSession;
use kernel::memory::TopTool;
use rusqlite::Connection;
use rusqlite::params;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const LIST_LIMIT: i64 = 12;

const TOTALS: &str = "SELECT
    (SELECT count(*) FROM ros_conversations WHERE ?1 IS NULL OR agent = ?1),
    (SELECT count(*) FROM ros_messages WHERE ?1 IS NULL OR agent = ?1),
    (SELECT count(*) FROM ros_messages
      WHERE (?1 IS NULL OR agent = ?1) AND (role = 'tool' OR tool_name IS NOT NULL)),
    (SELECT count(*) FROM ros_summaries
      WHERE ?1 IS NULL
         OR conversation_id IN (SELECT id FROM ros_conversations WHERE agent = ?1)),
    (SELECT count(*) FROM ros_jobs WHERE task = 'embed-target' AND state <> 'dead'),
    (SELECT count(embedding) FROM ros_messages WHERE ?1 IS NULL OR agent = ?1),
    (SELECT count(*) FROM ros_messages
      WHERE (?1 IS NULL OR agent = ?1) AND embed_status = 'failed')
  + (SELECT count(*) FROM ros_summaries
      WHERE embed_status = 'failed'
        AND (?1 IS NULL
             OR conversation_id IN (SELECT id FROM ros_conversations WHERE agent = ?1)))";

const TOP_TOOLS: &str = "SELECT tool_name, count(*) FROM ros_messages
  WHERE tool_name IS NOT NULL AND (?1 IS NULL OR agent = ?1)
  GROUP BY tool_name ORDER BY count(*) DESC, tool_name ASC LIMIT ?2";

const RECENT_SESSIONS: &str = "SELECT c.session_key, c.title, c.agent, c.updated_at,
       (SELECT count(*) FROM ros_messages m WHERE m.conversation_id = c.id)
  FROM ros_conversations c
 WHERE ?1 IS NULL OR c.agent = ?1
 ORDER BY c.updated_at DESC, c.rowid ASC LIMIT ?2";

const CAPTURE_ACTIVITY: &str =
    "SELECT substr(session_key, 1, instr(session_key, ':') - 1), max(updated_at)
  FROM ros_conversations WHERE instr(session_key, ':') > 1
 GROUP BY 1";

impl SqliteStore {
    pub async fn capture_activity_read(
        &self,
    ) -> Result<Vec<kernel::harness_setup::CaptureActivity>, SqliteError> {
        self.with_conn(|conn| {
            conn.prepare(CAPTURE_ACTIVITY)
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(SqliteError::query("capture activity"))?
                .into_iter()
                .map(|(key_prefix, updated_at)| {
                    Ok(kernel::harness_setup::CaptureActivity {
                        key_prefix,
                        last_at: time::stored(&updated_at, "updated_at")?,
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn stats_for(
        &self,
        agent: Option<String>,
    ) -> Result<MemoryStatsResponse, SqliteError> {
        self.with_conn(move |conn| read_stats(conn, agent.as_deref()))
            .await
    }
}

fn read_stats(conn: &Connection, agent: Option<&str>) -> Result<MemoryStatsResponse, SqliteError> {
    let totals = conn
        .query_row(TOTALS, [agent], |row| {
            Ok([
                row.get::<_, i64>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ])
        })
        .map_err(SqliteError::query("stats"))?;
    let [
        conversations,
        messages,
        tool_calls,
        summaries,
        queue,
        embedded,
        failed,
    ] = totals.map(count);
    let top_tools = conn
        .prepare(TOP_TOOLS)
        .and_then(|mut statement| {
            statement
                .query_map(params![agent, LIST_LIMIT], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(SqliteError::query("stats top tools"))?
        .into_iter()
        .map(|(tool, used)| {
            Ok(TopTool {
                tool,
                count: count(used)?,
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    let recent_sessions = conn
        .prepare(RECENT_SESSIONS)
        .and_then(|mut statement| {
            statement
                .query_map(params![agent, LIST_LIMIT], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(SqliteError::query("stats recent sessions"))?
        .into_iter()
        .map(|(session_id, title, agent, updated_at, messages)| {
            Ok(RecentSession {
                session_id,
                title,
                agent,
                last_active: time::stored(&updated_at, "updated_at")?,
                messages: count(messages)?,
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    Ok(MemoryStatsResponse {
        conversations: conversations?,
        messages: messages?,
        tool_calls: tool_calls?,
        summaries: summaries?,
        embed_queue_depth: queue?,
        embedded_messages: embedded?,
        failed_embeddings: failed?,
        top_tools,
        recent_sessions,
    })
}

fn count(value: i64) -> Result<u64, SqliteError> {
    u64::try_from(value).map_err(SqliteError::Count)
}
