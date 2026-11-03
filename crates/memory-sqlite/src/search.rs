use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::str::FromStr as _;

use chrono::DateTime;
use chrono::TimeZone;
use chrono::Utc;
use kernel::memory::MemoryDegraded;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::MemorySearchResult;
use kernel::memory::SearchMode;
use kernel::memory::SearchOutcome;
use kernel::memory::SearchQuery;
use kernel::memory::TagEntityType;
use kernel::memory::TagLiteral;
use kernel::memory_core::hybrid_pool;
use rusqlite::Connection;
use rusqlite::params;
use wire::message::Role;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::health::EMBED_MISSING;
use crate::time;

pub(crate) mod embed;
mod fts;
pub(crate) mod functions;
mod ranked;
pub(crate) use fts::fts_or;

const KEYWORD_EFFECT: &str = "Keyword / FTS ranking only \u{2014} not meaning-based.";
const SUMMARY_AGENT: &str = "summary";

const MESSAGE_SQL: &str = "SELECT m.id, m.content, m.role, m.agent, m.created_at, m.conversation_id, \
     ros_messages_fts.rank, \
     (SELECT c.session_key FROM ros_conversations c WHERE c.id = m.conversation_id), \
     (SELECT c.channel FROM ros_conversations c WHERE c.id = m.conversation_id) \
     FROM ros_messages_fts \
     JOIN ros_messages m ON m.id = ros_messages_fts.id \
     WHERE ros_messages_fts MATCH ?1 \
       AND (?2 IS NULL OR m.agent = ?2) \
       AND (?3 IS NULL OR m.created_at >= ?3) \
       AND (?4 IS NULL OR m.created_at < ?4) \
       AND (?6 IS NULL OR m.role = ?6) \
       AND (?7 IS NULL OR m.tool_name = ?7) \
       AND (?8 IS NULL OR m.role NOT IN (SELECT value FROM json_each(?8))) \
     ORDER BY ros_messages_fts.rank ASC \
     LIMIT ?5";

const SUMMARY_SQL: &str = "SELECT s.id, s.content, s.created_at, s.conversation_id, \
     ros_summaries_fts.rank, \
     (SELECT c.session_key FROM ros_conversations c WHERE c.id = s.conversation_id), \
     (SELECT c.channel FROM ros_conversations c WHERE c.id = s.conversation_id) \
     FROM ros_summaries_fts \
     JOIN ros_summaries s ON s.id = ros_summaries_fts.id \
     WHERE ros_summaries_fts MATCH ?1 \
       AND (?2 IS NULL OR EXISTS ( \
             SELECT 1 FROM ros_conversations c \
              WHERE c.id = s.conversation_id AND c.agent = ?2)) \
       AND (?3 IS NULL OR s.created_at >= ?3) \
       AND (?4 IS NULL OR s.created_at < ?4) \
     ORDER BY ros_summaries_fts.rank ASC \
     LIMIT ?5";

const TAGS_SQL: &str =
    "SELECT entity_type, entity_id, key, value FROM ros_tags WHERE state = 'accepted'";

const SUMMARY_LINKS_SQL: &str = "SELECT id, conversation_id FROM ros_summaries";

const MESSAGE_BUMP: &str = "UPDATE ros_messages \
     SET access_count = access_count + 1, last_accessed_at = ?1 \
     WHERE id = ?2";

const SUMMARY_BUMP: &str = "UPDATE ros_summaries \
     SET access_count = access_count + 1, last_accessed_at = ?1 \
     WHERE id = ?2";

#[derive(Debug, Clone)]
struct Plan {
    pattern: Option<String>,
    text: String,
    mode: SearchMode,
    scope: MemoryScope,
    agent: Option<String>,
    role: Option<Role>,
    exclude_roles: Option<String>,
    tool_name: Option<String>,
    tag: Option<TagLiteral>,
    since: Option<String>,
    before: Option<String>,
    limit: i64,
    fetch_limit: i64,
    now: DateTime<Utc>,
}

#[derive(Debug, Clone)]
enum Kind {
    Message {
        role: Role,
        agent: String,
        conversation_id: String,
        session_id: Option<String>,
    },
    Summary {
        conversation_id: Option<String>,
        session_id: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct Candidate {
    id: String,
    content: String,
    created_at: DateTime<Utc>,
    rank: f64,
    kind: Kind,
}

#[derive(Debug, Default)]
struct TagIndex {
    conversation: BTreeMap<String, BTreeSet<(String, String)>>,
    summary: BTreeMap<String, BTreeSet<(String, String)>>,
    summaries_of: BTreeMap<String, Vec<String>>,
}

impl SqliteStore {
    pub async fn search_at<Tz: TimeZone + Sync>(
        &self,
        query: SearchQuery,
        now: DateTime<Utc>,
        zone: &Tz,
    ) -> Result<SearchOutcome, SqliteError> {
        let plan = Plan::prepare(query, now, zone)?;
        let semantic = matches!(plan.mode, SearchMode::Hybrid | SearchMode::Vector);
        let (vector, degraded) = if semantic {
            match &self.query_embed {
                None => (None, Some(EMBED_MISSING.to_owned())),
                Some(embed) => match embed.vector(&plan.text).await {
                    Ok(vector) => (Some(vector), None),
                    Err(embed::EmbedFailure::Cancelled) => {
                        return Err(SqliteError::SearchCancelled);
                    }
                    Err(error) => {
                        self.record_log(format!("[memory.sqlite] query embedding failed, searching without the vector arm: {error}"));
                        (None, Some(error.to_string()))
                    }
                },
            }
        } else {
            (
                None,
                (plan.mode == SearchMode::Fts && self.query_embed.is_none())
                    .then(|| EMBED_MISSING.to_owned()),
            )
        };
        let keyword = plan.mode == SearchMode::Fts || (semantic && self.query_embed.is_none());
        self.with_conn(move |conn| {
            if plan.mode == SearchMode::Regex {
                functions::compile(&plan.text)?;
            }
            let mut outcome = if keyword {
                execute(conn, &plan)?
            } else {
                ranked::execute(conn, &plan, vector.as_deref())?
            };
            if let Some(reason) = degraded {
                outcome.degraded = Some(MemoryDegraded {
                    reason,
                    effect: KEYWORD_EFFECT.to_owned(),
                });
            }
            Ok(outcome)
        })
        .await
    }
}

impl Plan {
    fn prepare<Tz: TimeZone>(
        query: SearchQuery,
        now: DateTime<Utc>,
        zone: &Tz,
    ) -> Result<Self, SqliteError> {
        if query.synthesize == Some(true) {
            return Err(SqliteError::SynthesisUnconfigured);
        }
        let range = query.time_range(now, zone);
        if range
            .since
            .zip(range.before)
            .is_some_and(|(since, before)| since >= before)
        {
            return Err(SqliteError::InvertedRange);
        }
        let pattern = fts_or(&query.text);
        let exclude_roles = if query.exclude_roles.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&query.exclude_roles).map_err(|source| {
                    SqliteError::Encode {
                        column: "exclude_roles",
                        source,
                    }
                })?,
            )
        };
        let limit = i64::from(query.limit.get());
        let fetch_limit = if query.tag.is_some() {
            hybrid_pool(query.limit.get())
        } else {
            limit
        };
        Ok(Self {
            pattern,
            text: query.text,
            mode: query.mode,
            scope: query.scope,
            agent: query.agent,
            role: query.role,
            exclude_roles,
            tool_name: query.tool_name,
            tag: query.tag,
            since: range.since.map(time::stamp),
            before: range.before.map(time::stamp),
            limit,
            fetch_limit,
            now,
        })
    }
}

fn execute(conn: &mut Connection, plan: &Plan) -> Result<SearchOutcome, SqliteError> {
    let mut pool = fetch(conn, plan)?;
    pool.sort_by(|left, right| {
        left.rank
            .total_cmp(&right.rank)
            .then(left.id.cmp(&right.id))
    });
    let index = if plan.tag.is_some() {
        TagIndex::load(conn)?
    } else {
        TagIndex::default()
    };
    let mut selected: Vec<Candidate> = pool
        .iter()
        .filter(|row| match plan.tag.as_ref() {
            None => true,
            Some(tag) => row.tagged(tag, &index),
        })
        .cloned()
        .collect();
    let matched = !selected.is_empty();
    selected.truncate(hit_limit(plan.limit)?);
    let reinforced = if plan.tag.is_some() && matched {
        pool.as_slice()
    } else {
        selected.as_slice()
    };
    bump(conn, reinforced, &time::stamp(plan.now))?;
    let results = selected.into_iter().map(Candidate::into_result).collect();
    Ok(SearchOutcome {
        results,
        ..SearchOutcome::default()
    })
}

fn hit_limit(limit: i64) -> Result<usize, SqliteError> {
    usize::try_from(limit).map_err(SqliteError::Count)
}

fn fetch(conn: &Connection, plan: &Plan) -> Result<Vec<Candidate>, SqliteError> {
    if plan.pattern.is_none() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    if matches!(plan.scope, MemoryScope::Messages | MemoryScope::Both) {
        found.extend(fetch_messages(conn, plan)?);
    }
    if matches!(plan.scope, MemoryScope::Summaries | MemoryScope::Both) {
        found.extend(fetch_summaries(conn, plan)?);
    }
    Ok(found)
}

fn fetch_messages(conn: &Connection, plan: &Plan) -> Result<Vec<Candidate>, SqliteError> {
    let operation = "search messages";
    let mut statement = conn
        .prepare(MESSAGE_SQL)
        .map_err(SqliteError::query(operation))?;
    let rows = statement
        .query_map(
            params![
                plan.pattern,
                plan.agent,
                plan.since,
                plan.before,
                plan.fetch_limit,
                plan.role.map(|role| role.to_string()),
                plan.tool_name,
                plan.exclude_roles,
            ],
            read_message,
        )
        .map_err(SqliteError::query(operation))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query(operation))?;
    rows.into_iter().map(Candidate::from_message).collect()
}

fn fetch_summaries(conn: &Connection, plan: &Plan) -> Result<Vec<Candidate>, SqliteError> {
    let operation = "search summaries";
    let mut statement = conn
        .prepare(SUMMARY_SQL)
        .map_err(SqliteError::query(operation))?;
    let rows = statement
        .query_map(
            params![
                plan.pattern,
                plan.agent,
                plan.since,
                plan.before,
                plan.fetch_limit,
            ],
            read_summary,
        )
        .map_err(SqliteError::query(operation))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query(operation))?;
    rows.into_iter().map(Candidate::from_summary).collect()
}

struct MessageRow {
    id: String,
    content: String,
    role: Role,
    agent: String,
    created_at: String,
    conversation_id: String,
    rank: f64,
    session_id: Option<String>,
}

struct SummaryRow {
    id: String,
    content: String,
    created_at: String,
    conversation_id: Option<String>,
    rank: f64,
    session_id: Option<String>,
}

fn read_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    let channel: Option<String> = row.get(8)?;
    Ok(MessageRow {
        id: row.get(0)?,
        content: row.get(1)?,
        role: crate::message::stored_role(row, 2)?,
        agent: row.get(3)?,
        created_at: row.get(4)?,
        conversation_id: row.get(5)?,
        rank: row.get(6)?,
        session_id: row
            .get::<_, Option<String>>(7)?
            .map(|key| kernel::filing::session_key(channel.as_deref(), &key)),
    })
}

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRow> {
    let channel: Option<String> = row.get(6)?;
    Ok(SummaryRow {
        id: row.get(0)?,
        content: row.get(1)?,
        created_at: row.get(2)?,
        conversation_id: row.get(3)?,
        rank: row.get(4)?,
        session_id: row
            .get::<_, Option<String>>(5)?
            .map(|key| kernel::filing::session_key(channel.as_deref(), &key)),
    })
}

impl Candidate {
    fn from_message(row: MessageRow) -> Result<Self, SqliteError> {
        Ok(Self {
            id: row.id,
            content: row.content,
            created_at: time::stored(&row.created_at, "created_at")?,
            rank: row.rank,
            kind: Kind::Message {
                role: row.role,
                agent: row.agent,
                conversation_id: row.conversation_id,
                session_id: row.session_id,
            },
        })
    }

    fn from_summary(row: SummaryRow) -> Result<Self, SqliteError> {
        Ok(Self {
            id: row.id,
            content: row.content,
            created_at: time::stored(&row.created_at, "created_at")?,
            rank: row.rank,
            kind: Kind::Summary {
                conversation_id: row.conversation_id,
                session_id: row.session_id,
            },
        })
    }

    fn tagged(&self, tag: &TagLiteral, index: &TagIndex) -> bool {
        match &self.kind {
            Kind::Message {
                conversation_id, ..
            } => index.message_has(conversation_id, tag),
            Kind::Summary {
                conversation_id, ..
            } => index.summary_has(&self.id, conversation_id.as_deref(), tag),
        }
    }

    fn into_result(self) -> MemorySearchResult {
        let relevance_score = relevance(self.rank);
        match self.kind {
            Kind::Message {
                role,
                agent,
                session_id,
                ..
            } => MemorySearchResult {
                chunk: None,
                id: self.id,
                source: MemoryHitSource::Message,
                content: self.content,
                role: Some(role),
                agent: Some(agent),
                relevance_score,
                created_at: self.created_at,
                session_id,
                title: None,
            },
            Kind::Summary { session_id, .. } => MemorySearchResult {
                chunk: None,
                id: self.id,
                source: MemoryHitSource::Summary,
                content: self.content,
                role: None,
                agent: Some(SUMMARY_AGENT.to_owned()),
                relevance_score,
                created_at: self.created_at,
                session_id,
                title: None,
            },
        }
    }

    fn source(&self) -> MemoryHitSource {
        match self.kind {
            Kind::Message { .. } => MemoryHitSource::Message,
            Kind::Summary { .. } => MemoryHitSource::Summary,
        }
    }
}

fn relevance(rank: f64) -> f64 {
    rank.abs().mul_add(1.0, 1.0).recip()
}

impl TagIndex {
    fn load(conn: &Connection) -> Result<Self, SqliteError> {
        let mut index = Self::default();
        index.read_tags(conn)?;
        index.read_links(conn)?;
        Ok(index)
    }

    fn read_tags(&mut self, conn: &Connection) -> Result<(), SqliteError> {
        let rows = collect_tags(conn)?;
        for (entity_type, entity_id, key, value) in rows {
            self.insert_tag(entity_type, entity_id, key, value)?;
        }
        Ok(())
    }

    fn insert_tag(
        &mut self,
        entity_type: String,
        entity_id: String,
        key: String,
        value: String,
    ) -> Result<(), SqliteError> {
        let kind = TagEntityType::from_str(&entity_type)
            .map_err(|_| SqliteError::StoredTagType { value: entity_type })?;
        let pairs = match kind {
            TagEntityType::Conversation => &mut self.conversation,
            TagEntityType::Summary => &mut self.summary,
        };
        pairs.entry(entity_id).or_default().insert((key, value));
        Ok(())
    }

    fn read_links(&mut self, conn: &Connection) -> Result<(), SqliteError> {
        for (summary_id, conversation_id) in collect_links(conn)? {
            if let Some(conversation_id) = conversation_id {
                self.summaries_of
                    .entry(conversation_id)
                    .or_default()
                    .push(summary_id);
            }
        }
        Ok(())
    }

    fn message_has(&self, conversation_id: &str, tag: &TagLiteral) -> bool {
        pair_has(&self.conversation, conversation_id, tag)
            || self
                .summaries_of
                .get(conversation_id)
                .is_some_and(|ids| ids.iter().any(|id| pair_has(&self.summary, id, tag)))
    }

    fn summary_has(&self, id: &str, conversation_id: Option<&str>, tag: &TagLiteral) -> bool {
        pair_has(&self.summary, id, tag)
            || conversation_id
                .is_some_and(|conversation| pair_has(&self.conversation, conversation, tag))
    }
}

fn pair_has(
    map: &BTreeMap<String, BTreeSet<(String, String)>>,
    id: &str,
    tag: &TagLiteral,
) -> bool {
    map.get(id).is_some_and(|pairs| {
        pairs
            .iter()
            .any(|(key, value)| key == &tag.key && value == &tag.value)
    })
}

fn collect_tags(conn: &Connection) -> Result<Vec<(String, String, String, String)>, SqliteError> {
    let mut statement = conn
        .prepare(TAGS_SQL)
        .map_err(SqliteError::query("search tags"))?;
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })
        .map_err(SqliteError::query("search tags"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("search tags"))
}

fn collect_links(conn: &Connection) -> Result<Vec<(String, Option<String>)>, SqliteError> {
    let mut statement = conn
        .prepare(SUMMARY_LINKS_SQL)
        .map_err(SqliteError::query("search summary links"))?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(SqliteError::query("search summary links"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("search summary links"))
}

fn bump(conn: &mut Connection, rows: &[Candidate], at: &str) -> Result<(), SqliteError> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("search access"))?;
    for row in rows {
        bump_one(&tx, row, at)?;
    }
    tx.commit()
        .map_err(SqliteError::query("search access commit"))?;
    Ok(())
}

fn bump_one(tx: &rusqlite::Transaction<'_>, row: &Candidate, at: &str) -> Result<(), SqliteError> {
    let sql = match row.source() {
        MemoryHitSource::Message => MESSAGE_BUMP,
        MemoryHitSource::Summary => SUMMARY_BUMP,
    };
    let updated = tx
        .execute(sql, params![at, row.id])
        .map_err(SqliteError::query("search access"))?;
    if updated == 1 {
        Ok(())
    } else {
        Err(SqliteError::MissingHit { id: row.id.clone() })
    }
}
