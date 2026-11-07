use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStatsResponse;
use kernel::memory::RecentSession;
use kernel::memory::TopTool;

use crate::backfills::Backfill;
use crate::error::Error;

const TOP_TOOLS: usize = 12;

const SCANNED_GROUPS: &str = "select tool_name, count(*) as total,
        count(*) filter (where role = 'tool' or tool_name is not null) as tool_calls,
        count(embedding) as embedded,
        count(*) filter (where embedding is null and embed_status is distinct from 'unembeddable'
            and embed_status is distinct from 'failed'
            and (content <> '' or tool_result <> '')) as queued,
        count(*) filter (where embed_status = 'failed') as failed
   from rh_messages group by tool_name";

const COUNTED_GROUPS: &str =
    "select nullif(tool_name, '') as tool_name, sum(total)::bigint as total,
        sum(tool_calls)::bigint as tool_calls, sum(embedded)::bigint as embedded,
        sum(queued)::bigint as queued, sum(failed)::bigint as failed
   from rh_message_counts group by tool_name having sum(total) > 0";

const SCANNED_RECENT: &str = "select c.id::text as session_id, c.title, c.agent, c.updated_at,
        (select count(*) from rh_messages m where m.conversation_id = c.id) as messages
   from rh_conversations c order by c.updated_at desc limit 12";

const COUNTED_RECENT: &str = "select c.id::text as session_id, c.title, c.agent, c.updated_at,
        (select coalesce(sum(n.total), 0)::bigint from rh_message_counts n
          where n.conversation_id = c.id) as messages
   from rh_conversations c order by c.updated_at desc limit 12";

const CAPTURE_ACTIVITY: &str =
    "select split_part(session_key, ':', 1) as key_prefix, max(updated_at) as last_at
   from rh_conversations where position(':' in session_key) > 1 group by 1";

#[derive(sqlx::FromRow)]
struct ActivityRow {
    key_prefix: String,
    last_at: DateTime<Utc>,
}

#[derive(Default, sqlx::FromRow)]
struct ToolGroup {
    tool_name: Option<String>,
    total: i64,
    tool_calls: i64,
    embedded: i64,
    queued: i64,
    failed: i64,
}

#[derive(sqlx::FromRow)]
struct OtherTotals {
    conversations: i64,
    summaries: i64,
    queued: i64,
    failed: i64,
}

#[derive(sqlx::FromRow)]
struct RecentRow {
    session_id: String,
    title: Option<String>,
    agent: String,
    updated_at: DateTime<Utc>,
    messages: i64,
}

fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|source| Error::protocol(source.to_string()))
}

impl crate::PostgresMemory {
    pub async fn read_capture_activity(
        &self,
    ) -> Result<Vec<kernel::harness_setup::CaptureActivity>, Error> {
        let rows = sqlx::query_as::<_, ActivityRow>(CAPTURE_ACTIVITY)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("capture activity", source))?;
        Ok(rows
            .into_iter()
            .map(|row| kernel::harness_setup::CaptureActivity {
                key_prefix: row.key_prefix,
                last_at: row.last_at,
            })
            .collect())
    }

    pub async fn memory_stats(&self) -> Result<MemoryStatsResponse, Error> {
        let counted = self
            .backfills
            .done(&self.pool, Backfill::MessageCounts)
            .await?;
        let (groups, recent) = if counted {
            (COUNTED_GROUPS, COUNTED_RECENT)
        } else {
            (SCANNED_GROUPS, SCANNED_RECENT)
        };
        let groups = sqlx::query_as::<_, ToolGroup>(groups).fetch_all(&self.pool);
        let others = sqlx::query_as::<_, OtherTotals>(
            "select (select count(*) from rh_conversations) as conversations,
                    count(*) as summaries,
                    count(*) filter (where embedding is null and embed_status is distinct from 'unembeddable'
                        and embed_status is distinct from 'failed') as queued,
                    count(*) filter (where embed_status = 'failed') as failed
               from rh_summaries",
        )
        .fetch_one(&self.pool);
        let recent = sqlx::query_as::<_, RecentRow>(recent).fetch_all(&self.pool);
        let (groups, others, session_rows) = futures::future::try_join3(groups, others, recent)
            .await
            .map_err(|source| Error::operation("stats", source))?;
        let mut messages = ToolGroup::default();
        let mut top_tools = Vec::new();
        for group in groups {
            messages.total += group.total;
            messages.tool_calls += group.tool_calls;
            messages.embedded += group.embedded;
            messages.queued += group.queued;
            messages.failed += group.failed;
            if let Some(tool) = group.tool_name {
                top_tools.push(TopTool {
                    tool,
                    count: count(group.total)?,
                });
            }
        }
        top_tools.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tool.cmp(&b.tool)));
        top_tools.truncate(TOP_TOOLS);
        let recent_sessions = session_rows
            .into_iter()
            .map(|row| -> Result<RecentSession, Error> {
                Ok(RecentSession {
                    session_id: row.session_id,
                    title: row.title,
                    agent: row.agent,
                    last_active: row.updated_at,
                    messages: count(row.messages)?,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(MemoryStatsResponse {
            conversations: count(others.conversations)?,
            messages: count(messages.total)?,
            tool_calls: count(messages.tool_calls)?,
            summaries: count(others.summaries)?,
            embed_queue_depth: count(messages.queued + others.queued)?,
            embedded_messages: count(messages.embedded)?,
            failed_embeddings: count(messages.failed + others.failed)?,
            top_tools,
            recent_sessions,
        })
    }
}
