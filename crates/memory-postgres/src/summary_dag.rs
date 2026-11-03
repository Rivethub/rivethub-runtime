use std::collections::HashMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryMessageTally;
use kernel::memory::MemoryStoreError;
use kernel::memory::MemorySummaryNode;
use kernel::memory::SUMMARY_PREVIEW_BYTES;
use kernel::memory::SummaryCounts;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryOwner;
use kernel::memory::SummaryShape;
use kernel::memory::SummaryTree;
use kernel::memory::TreeWindow;
use kernel::memory::VectorState;
use uuid::Uuid;

use crate::PostgresMemory;
use crate::error::Error;

const SESSION_SQL: &str = "select id, session_key from rh_conversations where session_key = any($1)
    order by active is not false desc, updated_at desc, id limit 1";

const CONVERSATION_SQL: &str = "select id, session_key from rh_conversations where id = $1";

const DAG_SQL: &str = "with recursive dag(id, level) as (
        select id, 0 from rh_summaries where conversation_id = $1
        union
        select up.parent_id, dag.level + 1
          from dag
          cross join lateral (
              select s.parent_id from rh_summaries s where s.id = dag.id offset 0
          ) up
         where up.parent_id is not null
    ),
    levels as (select id, max(level) as level from dag group by id)
    select s.id::text as id, s.parent_id::text as parent_id, l.level::bigint as level, s.kind,
           case when $2::int is null then convert_to(s.content, 'UTF8')
                else substring(convert_to(s.content, 'UTF8') from 1 for $2) end as content,
           $2::int is not null and octet_length(s.content) > $2 as truncated,
           s.message_count, s.earliest_at, s.latest_at, s.created_at,
           case when $2::int is null
                then array(select ss.message_id::text from rh_summary_sources ss
                            where ss.summary_id = s.id order by ss.ordinal)
                else '{}'::text[] end as source_message_ids,
           case when s.embedding is not null then 'embedded'
                when s.embed_status = 'failed' then 'failed'
                when s.embed_status = 'unembeddable' then 'skipped'
                else 'pending' end as vector
    from levels l
    cross join lateral (select * from rh_summaries s where s.id = l.id offset 0) s
    where $3::uuid is null or l.id = $3
    order by l.level desc, s.latest_at, s.id";

const SHAPE_SQL: &str = "with recursive dag(id, level) as (
        select id, 0 from rh_summaries where conversation_id = $1
        union
        select up.parent_id, dag.level + 1
          from dag
          cross join lateral (
              select s.parent_id from rh_summaries s where s.id = dag.id offset 0
          ) up
         where up.parent_id is not null
    ),
    levels as (select id, max(level) as level from dag group by id)
    select s.id::text as id, s.parent_id::text as parent_id, l.level::bigint as level, s.kind,
           s.embedding is not null as embedded, s.earliest_at, s.latest_at
    from levels l
    cross join lateral (select * from rh_summaries s where s.id = l.id offset 0) s";

const PICKED_SQL: &str =
    "select s.id::text as id, s.parent_id::text as parent_id, 0::bigint as level,
           s.kind, substring(convert_to(s.content, 'UTF8') from 1 for $2) as content,
           octet_length(s.content) > $2 as truncated,
           s.message_count, s.earliest_at, s.latest_at, s.created_at,
           '{}'::text[] as source_message_ids,
           case when s.embedding is not null then 'embedded'
                when s.embed_status = 'failed' then 'failed'
                when s.embed_status = 'unembeddable' then 'skipped'
                else 'pending' end as vector
    from rh_summaries s where s.id = any($1)";

const LATEST_SQL: &str = "with picked as (
        select p.id from rh_summaries p where p.conversation_id = $1
        order by p.latest_at desc nulls last, (p.kind = 'leaf') desc, p.id desc
        limit $2
    )
    select s.id::text as id, s.parent_id::text as parent_id, s.depth::bigint as level, s.kind,
           convert_to(s.content, 'UTF8') as content, false as truncated,
           s.message_count, s.earliest_at, s.latest_at, s.created_at,
           '{}'::text[] as source_message_ids,
           case when s.embedding is not null then 'embedded'
                when s.embed_status = 'failed' then 'failed'
                when s.embed_status = 'unembeddable' then 'skipped'
                else 'pending' end as vector
    from picked join rh_summaries s on s.id = picked.id
    order by s.latest_at desc nulls last, (s.kind = 'leaf') desc, s.id desc";

const OWNED_SQL: &str =
    "select exists(select 1 from rh_summaries where id = $1 and conversation_id = $2)";

const CHILDREN_SQL: &str = "with recursive down(top, id, depth) as (
        select s.id, s.id, 0 from rh_summaries s where s.parent_id = $1
        union all
        select down.top, c.id, down.depth + 1
          from down join rh_summaries c on c.parent_id = down.id
    ),
    heights as (select top, max(depth) as level from down group by top)
    select s.id::text as id, s.parent_id::text as parent_id, h.level::bigint as level, s.kind,
           substring(convert_to(s.content, 'UTF8') from 1 for $2) as content,
           octet_length(s.content) > $2 as truncated,
           s.message_count, s.earliest_at, s.latest_at, s.created_at,
           '{}'::text[] as source_message_ids,
           case when s.embedding is not null then 'embedded'
                when s.embed_status = 'failed' then 'failed'
                when s.embed_status = 'unembeddable' then 'skipped'
                else 'pending' end as vector,
           (select count(*) from rh_summaries c where c.parent_id = s.id) as children
    from heights h join rh_summaries s on s.id = h.top
    order by coalesce(s.earliest_at, s.latest_at) nulls first, s.id::text";

const TALLY_SQL: &str = "select count(*) as total,
        count(*) filter (where role = 'user') as \"user\",
        count(*) filter (where role = 'assistant') as assistant,
        count(*) filter (where role = 'system') as \"system\",
        count(*) filter (where role = 'tool' or tool_name is not null) as tool,
        count(embedding) as embedded,
        count(*) filter (where embedding is null
                           and embed_status is distinct from 'failed'
                           and embed_status is distinct from 'unembeddable') as pending,
        count(*) filter (where embedding is null and embed_status = 'failed') as failed,
        count(*) filter (where embedding is null and embed_status = 'unembeddable') as skipped
    from rh_messages where conversation_id = $1";

#[derive(Debug, sqlx::FromRow)]
struct DagRow {
    id: String,
    parent_id: Option<String>,
    level: i64,
    kind: String,
    content: Vec<u8>,
    truncated: bool,
    message_count: i32,
    earliest_at: Option<DateTime<Utc>>,
    latest_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    source_message_ids: Vec<String>,
    vector: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ChildRow {
    #[sqlx(flatten)]
    node: DagRow,
    children: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ShapeRow {
    id: String,
    parent_id: Option<String>,
    level: i64,
    kind: String,
    embedded: bool,
    earliest_at: Option<DateTime<Utc>>,
    latest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct TallyRow {
    total: i64,
    user: i64,
    assistant: i64,
    system: i64,
    tool: i64,
    embedded: i64,
    pending: i64,
    failed: i64,
    skipped: i64,
}

#[derive(Debug, Clone, Copy)]
enum Detail {
    Full,
    Outline,
    One(Uuid),
}

impl PostgresMemory {
    pub(crate) async fn read_summary_dag(
        &self,
        owner: &SummaryOwner,
    ) -> Result<SummaryDag, MemoryStoreError> {
        self.dag(owner, Detail::Full).await
    }

    pub(crate) async fn read_summary_outline(
        &self,
        owner: &SummaryOwner,
    ) -> Result<SummaryDag, MemoryStoreError> {
        self.dag(owner, Detail::Outline).await
    }

    pub(crate) async fn read_summary_latest(
        &self,
        owner: &SummaryOwner,
        limit: usize,
    ) -> Result<SummaryDag, MemoryStoreError> {
        let Some((conversation, session_key)) = self.conversation_of(owner).await? else {
            return Ok(SummaryDag::default());
        };
        let rows = sqlx::query_as::<_, DagRow>(LATEST_SQL)
            .bind(conversation)
            .bind(i64::try_from(limit).map_err(count_error)?)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("latest summaries", source))?;
        Ok(SummaryDag {
            session_key: Some(session_key),
            summaries: rows
                .into_iter()
                .map(summary_node)
                .collect::<Result<_, _>>()?,
            messages: MemoryMessageTally::default(),
        })
    }

    pub(crate) async fn read_summary_node(
        &self,
        owner: &SummaryOwner,
        id: &str,
    ) -> Result<Option<MemorySummaryNode>, MemoryStoreError> {
        let id = Uuid::parse_str(id).map_err(|error| MemoryStoreError::Query {
            message: format!("summary id {id} is not a uuid"),
            source: Box::new(error),
        })?;
        Ok(self.dag(owner, Detail::One(id)).await?.summaries.pop())
    }

    pub(crate) async fn read_summary_tree(
        &self,
        owner: &SummaryOwner,
        parent: Option<&str>,
        window: TreeWindow,
    ) -> Result<Option<SummaryTree>, MemoryStoreError> {
        let Some((conversation, session_key)) = self.conversation_of(owner).await? else {
            return Ok(SummaryDag::default().tree(parent, window));
        };
        if let Some(parent) = parent
            && let Ok(id) = Uuid::parse_str(parent)
            && let Some(children) = self.owned_children(conversation, id, window).await?
        {
            return Ok(Some(SummaryTree {
                session_key: Some(session_key),
                ..children
            }));
        }
        let shapes = sqlx::query_as::<_, ShapeRow>(SHAPE_SQL)
            .bind(conversation)
            .fetch_all(&self.pool);
        let tally = async {
            if parent.is_some() {
                return Ok(None);
            }
            sqlx::query_as::<_, TallyRow>(TALLY_SQL)
                .bind(conversation)
                .fetch_one(&self.pool)
                .await
                .map(Some)
        };
        let (shapes, tally) = tokio::try_join!(shapes, tally)
            .map_err(|source| Error::operation("summary tree", source))?;
        let levels: HashMap<String, u32> = shapes
            .iter()
            .map(|row| {
                Ok((
                    row.id.clone(),
                    u32::try_from(row.level).map_err(count_error)?,
                ))
            })
            .collect::<Result<_, MemoryStoreError>>()?;
        let shapes = shapes
            .into_iter()
            .map(summary_shape)
            .collect::<Result<Vec<_>, _>>()?;
        let Some(cut) = kernel::memory::cut(&shapes, parent, window) else {
            return Ok(None);
        };
        let picked = self.picked(&cut.ids, &levels).await?;
        Ok(Some(SummaryTree {
            session_key: Some(session_key),
            summaries: cut.pick(picked),
            total: cut.total,
            counts: if parent.is_none() {
                cut.counts
            } else {
                SummaryCounts::default()
            },
            messages: tally
                .as_ref()
                .map(message_tally)
                .transpose()?
                .unwrap_or_default(),
        }))
    }

    async fn picked(
        &self,
        ids: &[String],
        levels: &HashMap<String, u32>,
    ) -> Result<Vec<MemorySummaryNode>, MemoryStoreError> {
        let ids = ids
            .iter()
            .map(|id| {
                Uuid::parse_str(id).map_err(|error| MemoryStoreError::Query {
                    message: format!("summary id {id} is not a uuid"),
                    source: Box::new(error),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let preview = i32::try_from(SUMMARY_PREVIEW_BYTES).map_err(count_error)?;
        sqlx::query_as::<_, DagRow>(PICKED_SQL)
            .bind(ids)
            .bind(preview)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("summary tree", source))?
            .into_iter()
            .map(|row| {
                let level = levels.get(&row.id).copied();
                summary_node(row).map(|node| MemorySummaryNode {
                    level: level.unwrap_or(node.level),
                    ..node
                })
            })
            .collect::<Result<Vec<_>, _>>()
    }

    async fn owned_children(
        &self,
        conversation: Uuid,
        parent: Uuid,
        window: TreeWindow,
    ) -> Result<Option<SummaryTree>, MemoryStoreError> {
        let owned: bool = sqlx::query_scalar(OWNED_SQL)
            .bind(parent)
            .bind(conversation)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("summary children", source))?;
        if !owned {
            return Ok(None);
        }
        let preview = i32::try_from(SUMMARY_PREVIEW_BYTES).map_err(count_error)?;
        let rows = sqlx::query_as::<_, ChildRow>(CHILDREN_SQL)
            .bind(parent)
            .bind(preview)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("summary children", source))?;
        let total = u64::try_from(rows.len()).map_err(count_error)?;
        let summaries = rows
            .into_iter()
            .skip(window.offset)
            .take(window.limit)
            .map(|row| {
                let children = u64::try_from(row.children).map_err(count_error)?;
                summary_node(row.node).map(|node| MemorySummaryNode {
                    children: Some(children),
                    ..node
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(SummaryTree {
            session_key: None,
            summaries,
            total,
            counts: SummaryCounts::default(),
            messages: MemoryMessageTally::default(),
        }))
    }

    async fn conversation_of(
        &self,
        owner: &SummaryOwner,
    ) -> Result<Option<(Uuid, String)>, MemoryStoreError> {
        match owner {
            SummaryOwner::Session(session) => {
                sqlx::query_as(SESSION_SQL).bind(kernel::filing::stored_keys(session))
            }
            SummaryOwner::Conversation(id) => sqlx::query_as(CONVERSATION_SQL).bind(*id),
        }
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("summary dag", source).into())
    }

    async fn dag(
        &self,
        owner: &SummaryOwner,
        detail: Detail,
    ) -> Result<SummaryDag, MemoryStoreError> {
        let Some((conversation, session_key)) = self.conversation_of(owner).await? else {
            return Ok(SummaryDag::default());
        };
        let (preview, only) = match detail {
            Detail::Full => (None, None),
            Detail::Outline => (
                Some(i32::try_from(SUMMARY_PREVIEW_BYTES).map_err(count_error)?),
                None,
            ),
            Detail::One(id) => (None, Some(id)),
        };
        let rows = sqlx::query_as::<_, DagRow>(DAG_SQL)
            .bind(conversation)
            .bind(preview)
            .bind(only)
            .fetch_all(&self.pool);
        let tally = async {
            if only.is_some() {
                return Ok(None);
            }
            sqlx::query_as::<_, TallyRow>(TALLY_SQL)
                .bind(conversation)
                .fetch_one(&self.pool)
                .await
                .map(Some)
        };
        let (rows, tally) = tokio::try_join!(rows, tally)
            .map_err(|source| Error::operation("summary dag", source))?;
        Ok(SummaryDag {
            session_key: Some(session_key),
            summaries: rows
                .into_iter()
                .map(summary_node)
                .collect::<Result<_, _>>()?,
            messages: tally
                .as_ref()
                .map(message_tally)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

fn text(bytes: Vec<u8>, truncated: bool) -> Result<String, MemoryStoreError> {
    String::from_utf8(bytes).or_else(|error| {
        let cut = error.utf8_error();
        if truncated && cut.error_len().is_none() {
            let valid = cut.valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(stored_text)
        } else {
            Err(stored_text(error))
        }
    })
}

fn stored_text(error: std::string::FromUtf8Error) -> MemoryStoreError {
    MemoryStoreError::Query {
        message: "a stored summary is not UTF-8".to_owned(),
        source: Box::new(error),
    }
}

fn summary_node(row: DagRow) -> Result<MemorySummaryNode, MemoryStoreError> {
    let kind = row
        .kind
        .parse::<SummaryKind>()
        .map_err(|error| MemoryStoreError::Query {
            message: format!(
                "stored summary kind {} is not leaf, branch, or root",
                row.kind
            ),
            source: Box::new(error),
        })?;
    let vector = row
        .vector
        .parse::<VectorState>()
        .map_err(|error| MemoryStoreError::Query {
            message: format!("summary vector state {} is not known", row.vector),
            source: Box::new(error),
        })?;
    Ok(MemorySummaryNode {
        id: row.id,
        parent_id: row.parent_id,
        level: u32::try_from(row.level).map_err(count_error)?,
        kind,
        content: text(row.content, row.truncated)?,
        message_count: u32::try_from(row.message_count).map_err(count_error)?,
        earliest_at: row.earliest_at,
        latest_at: row.latest_at,
        created_at: row.created_at,
        source_message_ids: row.source_message_ids,
        vector,
        truncated: row.truncated,
        children: None,
    })
}

fn summary_shape(row: ShapeRow) -> Result<SummaryShape, MemoryStoreError> {
    let kind = row
        .kind
        .parse::<SummaryKind>()
        .map_err(|error| MemoryStoreError::Query {
            message: format!(
                "stored summary kind {} is not leaf, branch, or root",
                row.kind
            ),
            source: Box::new(error),
        })?;
    Ok(SummaryShape {
        id: row.id,
        parent_id: row.parent_id,
        kind,
        embedded: row.embedded,
        earliest_at: row.earliest_at,
        latest_at: row.latest_at,
    })
}

fn message_tally(row: &TallyRow) -> Result<MemoryMessageTally, MemoryStoreError> {
    let count = |value: i64| u32::try_from(value).map_err(count_error);
    Ok(MemoryMessageTally {
        total: count(row.total)?,
        user: count(row.user)?,
        assistant: count(row.assistant)?,
        system: count(row.system)?,
        tool: count(row.tool)?,
        embedded: count(row.embedded)?,
        pending: count(row.pending)?,
        failed: count(row.failed)?,
        skipped: count(row.skipped)?,
    })
}

fn count_error(error: std::num::TryFromIntError) -> MemoryStoreError {
    MemoryStoreError::Query {
        message: "a stored summary dag count does not fit an unsigned count".to_owned(),
        source: Box::new(error),
    }
}
