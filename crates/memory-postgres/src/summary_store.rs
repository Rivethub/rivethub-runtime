use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::NewSummary;
use kernel::memory::PendingConversation;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::SummaryWrite;
use kernel::memory::ToolUse;
use kernel::memory::WindowMessage;
use sqlx::PgPool;
use sqlx::Row;
use std::str::FromStr;
use uuid::Uuid;
use wire::message::Role;

use crate::PostgresMemory;

const HEAD_SQL: &str = "select id, session_key, agent, channel, title
    from rh_conversations where id = $1";

const MESSAGES_SQL: &str = "select m.id, m.agent, m.role, m.content, m.created_at,
        case when m.role = 'tool'
             then convert_to(substr(coalesce(m.tool_args::text, ''), 1, $2), 'SQL_ASCII')
        end as tool_input,
        case when m.role = 'tool'
             then convert_to(substr(coalesce(m.tool_result, ''), 1, $3), 'SQL_ASCII')
        end as tool_result
    from rh_messages m
    where m.conversation_id = $1
      and not exists (
        select 1 from rh_summary_sources s where s.message_id = m.id
      )
    order by m.created_at, m.id";

const LATEST_SQL: &str = "select max(created_at) as latest_at
    from rh_messages where conversation_id = $1";

const NODES_SQL: &str = "select id, kind, content, message_count, earliest_at, latest_at, depth
    from rh_summaries
    where conversation_id = $1 and parent_id is null and kind = $2
    order by earliest_at, id";

const PENDING_SQL: &str = "select c.id, pending.unsummarized,
        (select max(m.created_at) from rh_messages m where m.conversation_id = c.id) as latest_at
    from (
        select m.conversation_id, count(*) as unsummarized
        from rh_messages m
        where not exists (
            select 1 from rh_summary_sources s where s.message_id = m.id
        )
        group by m.conversation_id
    ) pending
    join rh_conversations c on c.id = pending.conversation_id
    where c.session_key not like 'heartbeat:%'
    order by latest_at desc nulls last, c.id";

const INSERT_SQL: &str = "insert into rh_summaries (
        id, conversation_id, depth, content, kind, message_count,
        earliest_at, latest_at, model, created_at, pipeline_version, owner_user_id
    ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
              (select owner_user_id from rh_conversations where id = $2))";

const NOTIFY_SQL: &str = "select pg_notify('rh_summary_written', json_build_object(
        'id', $1::text, 'sessionKey', c.session_key, 'kind', $2::text, 'depth', $3::int)::text)
    from rh_conversations c where c.id = $4";

const SOURCE_SQL: &str = "insert into rh_summary_sources (summary_id, message_id, ordinal)
    values ($1, $2, $3)";

const LEAVES_SQL: &str = "select count(*) from rh_summaries
    where conversation_id = $1 and kind = $2";

const PARENT_SQL: &str = "update rh_summaries
    set parent_id = $1
    where id = $2 and conversation_id = $3";

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct HeadRow {
    id: Uuid,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
}

impl From<HeadRow> for ConversationHead {
    fn from(row: HeadRow) -> Self {
        Self {
            id: row.id,
            session_key: row.session_key,
            agent: row.agent,
            channel: row.channel,
            title: row.title,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    kind: String,
    content: String,
    message_count: i32,
    earliest_at: Option<DateTime<Utc>>,
    latest_at: Option<DateTime<Utc>>,
    depth: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct LatestRow {
    latest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct PendingRow {
    id: Uuid,
    unsummarized: i64,
    latest_at: Option<DateTime<Utc>>,
}

#[async_trait]
impl SummaryStore for PostgresMemory {
    async fn conversation(
        &self,
        id: Uuid,
    ) -> Result<Option<ConversationWindow>, SummaryStoreError> {
        load_window(&self.pool, id).await
    }

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError> {
        sqlx::query_as::<_, PendingRow>(PENDING_SQL)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| query_err("list pending conversations", source))?
            .into_iter()
            .map(|row| {
                Ok(PendingConversation {
                    id: row.id,
                    unsummarized: u32::try_from(row.unsummarized)
                        .map_err(|_| SummaryStoreError::Count)?,
                    latest_message_at: row.latest_at,
                })
            })
            .collect()
    }

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError> {
        let message_count =
            i32::try_from(summary.message_count).map_err(|_| SummaryStoreError::Count)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| query_err("begin summary write", source))?;
        sqlx::query(INSERT_SQL)
            .bind(summary.id)
            .bind(summary.conversation_id)
            .bind(summary.depth)
            .bind(&summary.content)
            .bind(summary.kind.as_str())
            .bind(message_count)
            .bind(summary.earliest_at)
            .bind(summary.latest_at)
            .bind(&summary.model)
            .bind(summary.created_at)
            .bind(summary.pipeline_version)
            .execute(&mut *tx)
            .await
            .map_err(|source| query_err("insert summary", source))?;
        for source in &summary.sources {
            sqlx::query(SOURCE_SQL)
                .bind(summary.id)
                .bind(source.message_id)
                .bind(source.ordinal)
                .execute(&mut *tx)
                .await
                .map_err(|source| query_err("insert summary source", source))?;
        }
        for child in &summary.child_ids {
            let updated = sqlx::query(PARENT_SQL)
                .bind(summary.id)
                .bind(child)
                .bind(summary.conversation_id)
                .execute(&mut *tx)
                .await
                .map_err(|source| query_err("link summary parent", source))?;
            if updated.rows_affected() != 1 {
                return Err(SummaryStoreError::Missing {
                    id: child.to_string(),
                });
            }
        }
        let written = if summary.kind == SummaryKind::Leaf {
            crate::title::title_from_first_leaf(&mut tx, summary.conversation_id)
                .await
                .map_err(|error| SummaryStoreError::Query {
                    message: error.to_string(),
                    source: Box::new(error),
                })?;
            let leaves: i64 = sqlx::query_scalar(LEAVES_SQL)
                .bind(summary.conversation_id)
                .bind(SummaryKind::Leaf.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(|source| query_err("count leaves", source))?;
            if leaves == 1 {
                SummaryWrite::FirstLeaf
            } else {
                SummaryWrite::Added
            }
        } else {
            SummaryWrite::Added
        };
        sqlx::query(NOTIFY_SQL)
            .bind(summary.id.to_string())
            .bind(summary.kind.as_str())
            .bind(summary.depth)
            .bind(summary.conversation_id)
            .execute(&mut *tx)
            .await
            .map_err(|source| query_err("announce summary", source))?;
        tx.commit()
            .await
            .map_err(|source| query_err("commit summary write", source))?;
        Ok(written)
    }

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|source| query_err("acquire for retitle", source))?;
        crate::title::name_conversation(&mut conn, id, title)
            .await
            .map_err(|error| SummaryStoreError::Query {
                message: error.to_string(),
                source: Box::new(error),
            })
    }
}

async fn load_window(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ConversationWindow>, SummaryStoreError> {
    let Some(head) = sqlx::query_as::<_, HeadRow>(HEAD_SQL)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|source| query_err("load conversation", source))?
    else {
        return Ok(None);
    };
    let messages = load_messages(pool, id).await?;
    let latest = sqlx::query_as::<_, LatestRow>(LATEST_SQL)
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|source| query_err("load latest message time", source))?;
    let leaves = load_nodes(pool, id, SummaryKind::Leaf).await?;
    let branches = load_nodes(pool, id, SummaryKind::Branch).await?;
    Ok(Some(ConversationWindow {
        conversation: head.into(),
        messages,
        leaves,
        branches,
        latest_message_at: latest.latest_at,
    }))
}

async fn load_nodes(
    pool: &PgPool,
    id: Uuid,
    kind: SummaryKind,
) -> Result<Vec<SummaryNode>, SummaryStoreError> {
    let rows = sqlx::query_as::<_, NodeRow>(NODES_SQL)
        .bind(id)
        .bind(kind.as_str())
        .fetch_all(pool)
        .await
        .map_err(|source| query_err("load summary nodes", source))?;
    rows.into_iter().map(node_from).collect()
}

async fn load_messages(pool: &PgPool, id: Uuid) -> Result<Vec<WindowMessage>, SummaryStoreError> {
    let rows = sqlx::query(MESSAGES_SQL)
        .bind(id)
        .bind(prefix_bytes(ToolUse::INPUT_BYTES)?)
        .bind(prefix_bytes(ToolUse::RESULT_BYTES)?)
        .fetch_all(pool)
        .await
        .map_err(|source| query_err("load unsummarized messages", source))?;
    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        messages.push(message_from(&row)?);
    }
    Ok(messages)
}

fn message_from(row: &sqlx::postgres::PgRow) -> Result<WindowMessage, SummaryStoreError> {
    let role_text: String = row
        .try_get("role")
        .map_err(|source| query_err("read message role", source))?;
    Ok(WindowMessage {
        id: row
            .try_get("id")
            .map_err(|source| query_err("read message id", source))?,
        agent: row
            .try_get("agent")
            .map_err(|source| query_err("read message agent", source))?,
        role: role_of(&role_text)?,
        content: row
            .try_get("content")
            .map_err(|source| query_err("read message content", source))?,
        tool: tool_use(row)?,
        created_at: row
            .try_get("created_at")
            .map_err(|source| query_err("read message time", source))?,
    })
}

fn tool_use(row: &sqlx::postgres::PgRow) -> Result<Option<ToolUse>, SummaryStoreError> {
    let input: Option<Vec<u8>> = row
        .try_get("tool_input")
        .map_err(|source| query_err("read tool input", source))?;
    let result: Option<Vec<u8>> = row
        .try_get("tool_result")
        .map_err(|source| query_err("read tool result", source))?;
    Ok(match (input, result) {
        (Some(input), Some(result)) => ToolUse::from_prefixes(&input, &result),
        _ => None,
    })
}

fn prefix_bytes(budget: usize) -> Result<i32, SummaryStoreError> {
    let bytes = i32::try_from(budget).map_err(|_| SummaryStoreError::Count)?;
    Ok(bytes.saturating_add(1))
}

fn role_of(text: &str) -> Result<Role, SummaryStoreError> {
    Role::from_str(text).map_err(|source| SummaryStoreError::Query {
        message: format!("stored message role {text}"),
        source: Box::new(source),
    })
}

fn node_from(row: NodeRow) -> Result<SummaryNode, SummaryStoreError> {
    let kind = kind_of(&row.kind)?;
    Ok(SummaryNode {
        id: row.id,
        kind,
        content: row.content,
        message_count: u32::try_from(row.message_count).map_err(|_| SummaryStoreError::Count)?,
        earliest_at: required_time(row.earliest_at, row.id)?,
        latest_at: required_time(row.latest_at, row.id)?,
        depth: row.depth,
    })
}

fn kind_of(kind: &str) -> Result<SummaryKind, SummaryStoreError> {
    match kind {
        "leaf" => Ok(SummaryKind::Leaf),
        "branch" => Ok(SummaryKind::Branch),
        "root" => Ok(SummaryKind::Root),
        other => Err(SummaryStoreError::Kind {
            kind: other.to_owned(),
        }),
    }
}

fn required_time(
    value: Option<DateTime<Utc>>,
    id: Uuid,
) -> Result<DateTime<Utc>, SummaryStoreError> {
    match value {
        Some(time) => Ok(time),
        None => Err(SummaryStoreError::Missing { id: id.to_string() }),
    }
}

fn query_err(operation: &'static str, source: sqlx::Error) -> SummaryStoreError {
    SummaryStoreError::Query {
        message: format!("{operation}: {source}"),
        source: Box::new(source),
    }
}
