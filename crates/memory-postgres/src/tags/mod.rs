use crate::error::Error;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::Limit;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagAction;
use kernel::memory::PendingTagWire;
use kernel::memory::TagAdd;
use kernel::memory::TagCount;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
use kernel::memory::TagWire;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::QueryBuilder;
use std::collections::BTreeMap;
use std::str::FromStr as _;
use uuid::Uuid;

mod suggest;
mod taxonomy;

#[derive(Debug, Clone, sqlx::FromRow)]
struct TagWireRow {
    pub(crate) id: String,
    pub(crate) entity_type: String,
    pub(crate) entity_id: String,
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) display: String,
    pub(crate) source: String,
    pub(crate) state: String,
    pub(crate) confidence: Option<f64>,
    pub(crate) proposed_by: String,
    pub(crate) reason: String,
    pub(crate) decided_by: Option<String>,
    pub(crate) decided_at: Option<DateTime<Utc>>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl TryFrom<TagWireRow> for TagWire {
    type Error = Error;

    fn try_from(row: TagWireRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            entity_type: entity_type_of(&row.entity_type)?,
            entity_id: row.entity_id,
            key: row.key,
            value: row.value,
            display: row.display,
            source: row.source,
            state: state_of(&row.state)?,
            confidence: row.confidence,
            proposed_by: row.proposed_by,
            reason: row.reason,
            decided_by: row.decided_by,
            decided_at: row.decided_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct LookupTagRow {
    session_key: String,
    #[sqlx(flatten)]
    tag: TagWireRow,
}

#[derive(Debug)]
struct PendingTagRow {
    id: String,
    entity_type: String,
    entity_id: String,
    key: String,
    value: String,
    display: String,
    source: String,
    state: String,
    confidence: Option<f64>,
    proposed_by: String,
    reason: String,
    decided_by: Option<String>,
    decided_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    is_removal: Option<bool>,
    session_key: Option<String>,
    title: Option<String>,
    agent: Option<String>,
    conversation_id: Option<String>,
    excerpt: Option<String>,
}

fn state_of(raw: &str) -> Result<TagState, Error> {
    TagState::from_str(raw).map_err(|source| Error::stored("tag state", source))
}

fn entity_type_of(raw: &str) -> Result<TagEntityType, Error> {
    TagEntityType::from_str(raw).map_err(|source| Error::stored("tag entity type", source))
}

enum TagTables<T> {
    Missing,
    Present(T),
}

impl<T> TagTables<Vec<T>> {
    fn shape(self) -> Vec<T> {
        match self {
            Self::Missing => Vec::new(),
            Self::Present(rows) => rows,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct TagCountRow {
    key: String,
    value: String,
    display: String,
    conversations: i64,
}

fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|source| Error::protocol(source.to_string()))
}

fn store_error(operation: &'static str, source: sqlx::Error) -> MemoryStoreError {
    if Error::relation_missing(&source) {
        return MemoryStoreError::TagTablesMissing;
    }
    if source
        .as_database_error()
        .is_some_and(|error| error.code().as_deref() == Some("23514"))
    {
        return MemoryStoreError::InvalidRequest {
            message: source.to_string(),
        };
    }
    MemoryStoreError::from(Error::operation(operation, source))
}

fn read_rows<T>(
    operation: &'static str,
    result: Result<T, sqlx::Error>,
) -> Result<TagTables<T>, MemoryStoreError> {
    match result {
        Ok(rows) => Ok(TagTables::Present(rows)),
        Err(source) if Error::relation_missing(&source) => Ok(TagTables::Missing),
        Err(source) => Err(store_error(operation, source)),
    }
}

fn push_wire_columns(builder: &mut QueryBuilder<Postgres>) {
    builder.push(
        "select t.id::text as id, t.entity_type, t.entity_id::text as entity_id, t.key, t.value, \
         t.display, t.source, t.state, t.confidence, t.proposed_by, t.reason, t.decided_by, \
         t.decided_at, t.created_at, t.updated_at from rh_tags t where true",
    );
}

pub(crate) async fn summary_conversation_tags(
    tx: &mut sqlx::PgConnection,
    summaries: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<TagWire>>, MemoryStoreError> {
    let mut builder = QueryBuilder::new("");
    push_wire_columns(&mut builder);
    builder
        .push(" and t.entity_type = ")
        .push_bind(TagEntityType::Conversation.to_string())
        .push(" and t.state = ")
        .push_bind(TagState::Accepted.to_string())
        .push(" and t.entity_id in (select conversation_id from rh_summaries where id = any(")
        .push_bind(summaries.to_vec())
        .push(")) order by t.entity_id, t.key, t.value");
    let rows = read_rows(
        "tags list",
        builder.build_query_as::<TagWireRow>().fetch_all(tx).await,
    )?;
    let mut grouped: BTreeMap<Uuid, Vec<TagWire>> = BTreeMap::new();
    for row in rows.shape() {
        let entity = Uuid::parse_str(&row.entity_id).map_err(Error::from)?;
        grouped
            .entry(entity)
            .or_default()
            .push(TagWire::try_from(row).map_err(MemoryStoreError::from)?);
    }
    Ok(grouped)
}

async fn list_tags(pool: &PgPool, query: &TagListQuery) -> Result<Vec<TagWire>, MemoryStoreError> {
    let mut builder = QueryBuilder::new("");
    push_wire_columns(&mut builder);
    if let Some(entity_type) = query.entity_type {
        builder
            .push(" and t.entity_type = ")
            .push_bind(entity_type.to_string());
    }
    if let Some(entity_id) = query.entity_id {
        builder.push(" and t.entity_id = ").push_bind(entity_id);
    }
    if let Some(key) = &query.key {
        builder.push(" and t.key = ").push_bind(key.clone());
    }
    if let Some(value) = &query.value {
        builder.push(" and t.value = ").push_bind(value.clone());
    }
    if !query.states.is_empty() {
        let states: Vec<String> = query.states.iter().map(ToString::to_string).collect();
        builder
            .push(" and t.state = any(")
            .push_bind(states)
            .push(")");
    }
    builder
        .push(if query.entity_id.is_some() {
            " order by t.key, t.value limit "
        } else {
            " order by t.created_at desc limit "
        })
        .push_bind(i64::from(query.limit.get()));
    let rows = read_rows(
        "tags list",
        builder.build_query_as::<TagWireRow>().fetch_all(pool).await,
    )?;
    rows.shape()
        .into_iter()
        .map(|row| TagWire::try_from(row).map_err(MemoryStoreError::from))
        .collect()
}

async fn pending_tags(
    pool: &PgPool,
    limit: Limit,
) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
    let rows = read_rows(
        "tags pending",
        sqlx::query_as!(
            PendingTagRow,
            "select t.id::text as \"id!\", t.entity_type, t.entity_id::text as \"entity_id!\", \
                 t.key, t.value, \
                t.display, t.source, t.state, t.confidence, t.proposed_by, \
                CASE WHEN t.removal_state = 'suggested' THEN t.removal_reason ELSE t.reason END AS \"reason!\", \
                t.decided_by, t.decided_at, t.created_at, t.updated_at, \
                (t.removal_state = 'suggested') as \"is_removal?\", \
                c.session_key as \"session_key?\", c.title as \"title?\", c.agent as \"agent?\", c.id::text as \"conversation_id?\", \
                (select left(m.content, 120) from rh_messages m
                  where m.conversation_id = c.id order by m.created_at asc limit 1) as excerpt \
           from rh_tags t \
           left join rh_summaries s on t.entity_type = 'summary' and s.id = t.entity_id \
           left join rh_conversations c on (t.entity_type = 'conversation' and c.id = t.entity_id) OR c.id = s.conversation_id \
          where t.state = 'suggested' or t.removal_state = 'suggested' \
          order by t.created_at desc \
          limit $1",
            i64::from(limit.get())
        )
        .fetch_all(pool)
        .await,
    )?
    .shape();
    rows.into_iter()
        .map(|row| -> Result<PendingTagWire, MemoryStoreError> {
            let tag = TagWire::try_from(TagWireRow {
                id: row.id,
                entity_type: row.entity_type,
                entity_id: row.entity_id,
                key: row.key,
                value: row.value,
                display: row.display,
                source: row.source,
                state: row.state,
                confidence: row.confidence,
                proposed_by: row.proposed_by,
                reason: row.reason,
                decided_by: row.decided_by,
                decided_at: row.decided_at,
                created_at: row.created_at,
                updated_at: row.updated_at,
            })
            .map_err(MemoryStoreError::from)?;
            Ok(PendingTagWire {
                tag,
                action: if row.is_removal == Some(true) {
                    Some(PendingTagAction::Remove)
                } else {
                    Some(PendingTagAction::Add)
                },
                session_key: row.session_key,
                title: row.title,
                agent: row.agent,
                conversation_id: row.conversation_id,
                excerpt: row.excerpt,
            })
        })
        .collect()
}

async fn tag_counts(
    pool: &PgPool,
    query: &TagCountsQuery,
) -> Result<Vec<TagCount>, MemoryStoreError> {
    let mut builder = QueryBuilder::new(
        "select key, value, max(display) as display, \
         count(*) filter (where entity_type = 'conversation')::bigint as conversations \
         from rh_tags where state = 'accepted'",
    );
    if let Some(key) = &query.key {
        builder.push(" and key = ").push_bind(key.clone());
    }
    builder
        .push(" group by key, value order by conversations desc, value limit ")
        .push_bind(i64::from(query.limit.get()));
    let rows = read_rows(
        "tag counts",
        builder
            .build_query_as::<TagCountRow>()
            .fetch_all(pool)
            .await,
    )?
    .shape();
    rows.into_iter()
        .map(|row| -> Result<TagCount, MemoryStoreError> {
            Ok(TagCount {
                key: row.key,
                value: row.value,
                display: row.display,
                conversations: count(row.conversations).map_err(MemoryStoreError::from)?,
            })
        })
        .collect()
}

async fn decide_tags(
    pool: &PgPool,
    ids: &[TagId],
    decision: TagDecision,
    decided_by: Option<String>,
) -> Result<u64, MemoryStoreError> {
    let uuids: Vec<Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let state = TagState::from(decision);
    let result = sqlx::query!(
        "update rh_tags set state = $2, \
           source = case when source = 'rule' and $2 = 'accepted' then 'user' else source end, \
           decided_by = coalesce($3, decided_by), decided_at = now(), updated_at = now() \
         where id = any($1)",
        &uuids,
        state.to_string(),
        decided_by
    )
    .execute(pool)
    .await
    .map_err(|source| store_error("tags decide", source))?;
    Ok(result.rows_affected())
}

async fn resolve_session(
    pool: &PgPool,
    key: &str,
    agent: Option<&str>,
) -> Result<Uuid, MemoryStoreError> {
    let rows = sqlx::query!(
        "select id from rh_conversations
          where session_key = $1 and ($2::text is null or agent = $2)",
        key,
        agent
    )
    .fetch_all(pool)
    .await
    .map_err(|source| store_error("session lookup", source))?;
    let conversation = match rows.as_slice() {
        [single] => single.id,
        [] => {
            return Err(MemoryStoreError::InvalidRequest {
                message: format!("no conversation captured for session \"{key}\""),
            });
        }
        _ => {
            return Err(MemoryStoreError::InvalidRequest {
                message: format!(
                    "invalid request: session \"{key}\" exists under several agents; pass agent"
                ),
            });
        }
    };
    Ok(conversation)
}

async fn add_tag(pool: &PgPool, add: &TagAdd) -> Result<TagWire, MemoryStoreError> {
    let entity_id = match &add.entity {
        TagEntity::Id(id) => *id,
        TagEntity::SessionKey(key) => resolve_session(pool, key, add.agent.as_deref()).await?,
    };
    let display = match add
        .display
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => text.to_owned(),
        None => add.tag.value.clone(),
    };
    let record = sqlx::query!(
        "insert into rh_tags (entity_type, entity_id, key, value, display, source, state, \
                              proposed_by, reason, decided_by, decided_at) \
         values ($1, $2, $3, $4, coalesce($5, ''), 'user', 'accepted', '', coalesce($6, ''), \
                 $7::text, case when $7::text is null then null else now() end) \
         on conflict (entity_type, entity_id, key, value) do update set state = 'accepted', \
           decided_by = coalesce($7::text, rh_tags.decided_by), \
           decided_at = case when $7::text is null then rh_tags.decided_at else now() end, \
           updated_at = now() \
         returning id::text as \"id!\", entity_type, entity_id::text as \"entity_id!\", key, value, \
                   display, source, state, confidence, proposed_by, reason, decided_by, \
                   decided_at, created_at, updated_at",
        add.entity_type.to_string(),
        entity_id,
        add.tag.key.as_str(),
        add.tag.value.as_str(),
        Some(display),
        add.reason.clone(),
        add.decided_by.clone()
    )
    .fetch_one(pool)
    .await
    .map_err(|source| store_error("tag add", source))?;
    Ok(TagWire {
        id: record.id,
        entity_type: add.entity_type,
        entity_id: record.entity_id,
        key: record.key,
        value: record.value,
        display: record.display,
        source: record.source,
        state: state_of(&record.state)?,
        confidence: record.confidence,
        proposed_by: record.proposed_by,
        reason: record.reason,
        decided_by: record.decided_by,
        decided_at: record.decided_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

async fn lookup_tags(
    pool: &PgPool,
    lookup: &TagLookup,
) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
    if lookup.session_keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut builder = QueryBuilder::new(
        "select c.session_key, t.id::text as id, t.entity_type, t.entity_id::text as entity_id, \
         t.key, t.value, t.display, t.source, t.state, t.confidence, t.proposed_by, t.reason, \
         t.decided_by, t.decided_at, t.created_at, t.updated_at \
         from rh_tags t join rh_conversations c on c.id = t.entity_id where true",
    );
    builder
        .push(" and t.entity_type = 'conversation' and c.session_key = any(")
        .push_bind(lookup.session_keys.clone())
        .push(")");
    if !lookup.states.is_empty() {
        let states: Vec<String> = lookup.states.iter().map(ToString::to_string).collect();
        builder
            .push(" and t.state = any(")
            .push_bind(states)
            .push(")");
    }
    let rows = read_rows(
        "tags lookup",
        builder
            .build_query_as::<LookupTagRow>()
            .fetch_all(pool)
            .await,
    )?
    .shape();
    let mut grouped = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.session_key)
            .or_insert_with(Vec::new)
            .push(TagWire::try_from(row.tag)?);
    }
    Ok(grouped)
}

impl crate::PostgresMemory {
    pub(crate) async fn tags_list(
        &self,
        query: &TagListQuery,
    ) -> Result<Vec<TagWire>, MemoryStoreError> {
        list_tags(&self.pool, query).await
    }

    pub(crate) async fn tags_pending(
        &self,
        limit: Limit,
    ) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
        pending_tags(&self.pool, limit).await
    }

    pub(crate) async fn tags_counts(
        &self,
        query: &TagCountsQuery,
    ) -> Result<Vec<TagCount>, MemoryStoreError> {
        tag_counts(&self.pool, query).await
    }

    pub(crate) async fn tags_decide(
        &self,
        ids: &[TagId],
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        decide_tags(&self.pool, ids, decision, decided_by).await
    }

    pub(crate) async fn tags_add(&self, add: &TagAdd) -> Result<TagWire, MemoryStoreError> {
        add_tag(&self.pool, add).await
    }

    pub(crate) async fn tags_lookup(
        &self,
        lookup: &TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
        lookup_tags(&self.pool, lookup).await
    }

    pub(crate) async fn tags_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<kernel::memory::TagJobContext>, MemoryStoreError> {
        suggest::tag_job_context(&self.pool, summary_id).await
    }

    pub(crate) async fn tags_store_suggestions(
        &self,
        write: &kernel::memory::TagSuggestionWrite,
    ) -> Result<kernel::memory::TagSuggestionStored, MemoryStoreError> {
        suggest::store_tag_suggestions(&self.pool, write).await
    }
}
