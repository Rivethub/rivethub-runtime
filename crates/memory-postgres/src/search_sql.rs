mod arms;

use crate::error::Error;
use crate::search::ArmKind;
use crate::search::ArmRow;
use crate::search::TagFilter;
use crate::search::{SearchOptions, SearchRead};
use crate::search_literal::Patience;
pub(crate) use arms::ArmContext;
use jiff::Timestamp;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory_core::recency_boost;
use kernel::memory_core::{hybrid_pool, literal_tokens};
use pgvector::HalfVector;
use sqlx::{Acquire as _, QueryBuilder};
use std::str::FromStr as _;
use wire::message::Role;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchTable {
    Message,
    Legacy,
    Passage,
}

impl SearchTable {
    pub(crate) const fn is_message(self) -> bool {
        matches!(self, Self::Message)
    }

    pub(crate) const fn alias(self) -> &'static str {
        match self {
            Self::Message => "m",
            Self::Legacy => "s",
            Self::Passage => "p",
        }
    }

    pub(crate) const fn from(self) -> &'static str {
        match self {
            Self::Message => "rh_messages m",
            Self::Legacy => "rh_summaries s",
            Self::Passage => "rh_summary_passages p JOIN rh_summaries s ON s.id = p.record_id",
        }
    }

    pub(crate) const fn time(self) -> &'static str {
        match self {
            Self::Message => "m.created_at",
            Self::Legacy => "s.created_at",
            Self::Passage => "p.latest_at",
        }
    }

    pub(crate) const fn newest(self) -> &'static str {
        match self {
            Self::Legacy => "s.latest_at",
            Self::Message | Self::Passage => self.time(),
        }
    }

    pub(crate) fn push_eligibility(self, query: &mut QueryBuilder<sqlx::Postgres>) {
        query.push(match self {
            Self::Message => "",
            Self::Legacy => " AND s.kind IN ('leaf', 'branch', 'root')
                AND NOT EXISTS (SELECT 1 FROM rh_summaries session
                                WHERE session.conversation_id = s.conversation_id AND session.kind = 'session')",
            Self::Passage => " AND s.kind IN ('session', 'part') AND NOT s.index_needs_repair",
        });
    }
}

pub(crate) fn semantic_floor(alias: &str) -> String {
    format!("least(length(btrim({alias}.content)) / 1000.0, 1.0)")
}

pub(crate) fn recency_columns(table: SearchTable) -> String {
    let alias = alias_of(table.is_message());
    let time = table.time();
    let (importance, access_table, access_key) = if table.is_message() {
        (
            format!(
                "(CASE WHEN {alias}.role = 'system' THEN 0.5 \
                        WHEN {alias}.tool_name IS NOT NULL THEN 0.3 \
                        WHEN {alias}.role = 'user' THEN 0.7 \
                        ELSE 0.6 END)"
            ),
            "rh_message_access",
            "message_id",
        )
    } else {
        ("0.6".to_string(), "rh_summary_access", "summary_id")
    };
    format!(
        "(EXTRACT(EPOCH FROM (now() - {time})) / 86400.0)::float8 AS age_days, \
         COALESCE((SELECT a.access_count FROM {access_table} a WHERE a.{access_key} = {alias}.id), \
                  {alias}.access_count, 0) AS access_count, \
         ({importance})::float8 AS importance"
    )
}

fn summary_kind(raw: Option<String>) -> Result<Option<kernel::memory::SummaryKind>, Error> {
    raw.map(|kind| {
        kind.parse()
            .map_err(|source| Error::stored("summary kind", source))
    })
    .transpose()
}

fn role_of(raw: Option<String>) -> Result<Option<Role>, Error> {
    raw.map(|role| Role::from_str(&role).map_err(|source| Error::stored("message role", source)))
        .transpose()
}

impl TagFilter {
    pub(crate) fn push_condition(
        &self,
        query: &mut QueryBuilder<sqlx::Postgres>,
        is_message: bool,
    ) {
        let (outer, inner) = if is_message {
            ("m.conversation_id", "conversation")
        } else {
            ("s.id", "summary")
        };
        query.push(format!(
            "{outer} IN (SELECT entity_id FROM rh_tags WHERE key = "
        ));
        query.push_bind(self.key.clone());
        query.push(" AND value = ");
        query.push_bind(self.value.clone());
        query.push(format!(
            " AND state = 'accepted' AND entity_type = '{inner}')"
        ));
    }
}

pub(crate) fn push_time_conditions(
    query: &mut QueryBuilder<sqlx::Postgres>,
    options: &SearchOptions,
    table: SearchTable,
) {
    let time = table.time();
    if let Some(since) = options.since {
        query.push(format!(" AND {time} >= "));
        query.push_bind(crate::time::BindTime::bind_time(since));
    }
    if let Some(before) = options.before {
        query.push(format!(" AND {time} < "));
        query.push_bind(crate::time::BindTime::bind_time(before));
    }
}

pub(crate) fn push_scope_conditions(
    query: &mut QueryBuilder<sqlx::Postgres>,
    options: &SearchOptions,
    is_message: bool,
) {
    query.push(crate::session_marks::not_trashed(alias_of(is_message)));
    push_option_conditions(query, options, is_message);
}

pub(crate) const fn alias_of(is_message: bool) -> &'static str {
    if is_message { "m" } else { "s" }
}

pub(crate) fn push_option_conditions(
    query: &mut QueryBuilder<sqlx::Postgres>,
    options: &SearchOptions,
    is_message: bool,
) {
    if let Some(agent) = &options.agent {
        if is_message {
            query.push(" AND m.agent = ");
            query.push_bind(agent.clone());
        } else {
            query.push(
                " AND EXISTS (SELECT 1 FROM rh_conversations c \
                 WHERE c.id = s.conversation_id AND c.agent = ",
            );
            query.push_bind(agent.clone());
            query.push(")");
        }
    }
    if !is_message {
        return;
    }
    if let Some(role) = options.role {
        query.push(" AND m.role = ");
        query.push_bind(role.to_string());
    }
    if !options.exclude_roles.is_empty() {
        query.push(" AND m.role <> ALL(");
        query.push_bind(
            options
                .exclude_roles
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        );
        query.push(")");
    }
    if let Some(tool_name) = &options.tool_name {
        query.push(" AND m.tool_name = ");
        query.push_bind(tool_name.clone());
    }
}

fn like_escaped(token: &str) -> String {
    let mut escaped = String::with_capacity(token.len());
    for character in token.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiteralText {
    Summary,
    Passage,
    Message,
    MessageAndResult,
}

impl LiteralText {
    pub(crate) fn of(options: &SearchOptions, table: SearchTable) -> Self {
        match table {
            SearchTable::Legacy => Self::Summary,
            SearchTable::Passage => Self::Passage,
            SearchTable::Message if options.admits(Role::Tool) => Self::MessageAndResult,
            SearchTable::Message => Self::Message,
        }
    }

    const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Summary => &["s.content"],
            Self::Passage => &["p.content"],
            Self::Message => &["m.content"],
            Self::MessageAndResult => &["m.content", "coalesce(m.tool_result, '')"],
        }
    }
}

fn case_free(token: &str) -> bool {
    token
        .bytes()
        .all(|byte| byte.is_ascii() && !byte.is_ascii_alphabetic())
}

pub(crate) fn push_contains(query: &mut QueryBuilder<sqlx::Postgres>, column: &str, token: &str) {
    let operator = if case_free(token) { "LIKE" } else { "ILIKE" };
    query.push(format!("{column} {operator} '%' || "));
    query.push_bind(like_escaped(token));
    query.push(" || '%' ESCAPE '\\'");
}

pub(crate) fn push_held(query: &mut QueryBuilder<sqlx::Postgres>, text: LiteralText, token: &str) {
    query.push("(");
    for (index, column) in text.columns().iter().enumerate() {
        if index > 0 {
            query.push(" OR ");
        }
        push_contains(query, column, token);
    }
    query.push(")");
}

fn push_any_contains(query: &mut QueryBuilder<sqlx::Postgres>, column: &str, tokens: &[String]) {
    query.push("(");
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            query.push(" OR ");
        }
        push_contains(query, column, token);
    }
    query.push(")");
}

fn push_literal_score(
    query: &mut QueryBuilder<sqlx::Postgres>,
    tokens: &[String],
    text: LiteralText,
) {
    query.push("((0");
    for token in tokens {
        query.push(" + (CASE WHEN ");
        push_held(query, text, token);
        query.push(" THEN 1 ELSE 0 END)");
    }
    query.push(format!(")::float8 / {})", tokens.len()));
}

fn push_literal_match(
    query: &mut QueryBuilder<sqlx::Postgres>,
    tokens: &[String],
    text: LiteralText,
) {
    let summary_column = match text {
        LiteralText::Summary => Some("s.content"),
        LiteralText::Passage => Some("p.content"),
        LiteralText::Message | LiteralText::MessageAndResult => None,
    };
    if let Some(column) = summary_column {
        query.push(" AND ");
        push_any_contains(query, column, tokens);
        return;
    }
    query.push(" AND m.id IN (SELECT hit.id FROM rh_messages hit WHERE ");
    push_any_contains(query, "hit.content", tokens);
    if text == LiteralText::MessageAndResult {
        query.push(
            " UNION SELECT hit.id FROM rh_messages hit \
             WHERE hit.tool_result IS NOT NULL AND length(hit.tool_result) > 0 AND ",
        );
        push_any_contains(query, "hit.tool_result", tokens);
    }
    query.push(")");
}

fn push_pattern_match(query: &mut QueryBuilder<sqlx::Postgres>, pattern: &str, text: LiteralText) {
    query.push(" AND (");
    for (index, column) in text.columns().iter().enumerate() {
        if index > 0 {
            query.push(" OR ");
        }
        query.push(format!("{column} ~* "));
        query.push_bind(pattern.to_owned());
    }
    query.push(")");
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ArmSqlRow {
    chunk: Option<sqlx::types::Json<kernel::memory::MemoryChunkMatch>>,
    id: String,
    record_id: Option<String>,
    passage_ordinal: Option<i32>,
    content_key: String,
    reasoning: bool,
    role_or_kind: Option<String>,
    agent: Option<String>,
    tool_name: Option<String>,
    conversation_id: Option<String>,
    session_id: Option<String>,
    session_channel: Option<String>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    #[sqlx(try_from = "crate::time::OptionalTimestamp")]
    newest: Option<Timestamp>,
    score: f64,
    semantic: f64,
    age_days: f64,
    #[sqlx(try_from = "i32")]
    access_count: u32,
    importance: f64,
}

impl ArmSqlRow {
    pub(crate) fn into_arm(self, is_message: bool) -> Result<ArmRow, Error> {
        let (role, kind) = if is_message {
            (role_of(self.role_or_kind)?, None)
        } else {
            (None, summary_kind(self.role_or_kind)?)
        };
        Ok(ArmRow {
            chunk: self.chunk.map(|chunk| chunk.0),
            passage: self
                .passage_ordinal
                .map(|ordinal| {
                    Ok::<_, Error>(kernel::memory::SummaryPassage {
                        id: self.id.clone(),
                        ordinal: u32::try_from(ordinal)?,
                    })
                })
                .transpose()?,
            id: self.id,
            record_id: self.record_id,
            reasoning: self.reasoning,
            source: if is_message {
                MemoryHitSource::Message
            } else {
                MemoryHitSource::Summary
            },
            content_key: self.content_key,
            role,
            agent: self.agent,
            kind,
            tool_name: self.tool_name,
            conversation_id: self.conversation_id,
            session_id: self
                .session_id
                .map(|key| kernel::filing::session_key(self.session_channel.as_deref(), &key)),
            created_at: self.created_at,
            newest: self.newest,
            arm_score: self.score,
            semantic: self.semantic,
            boost: recency_boost(self.age_days, self.access_count, self.importance),
        })
    }
}

pub(crate) fn push_select_prefix(
    query: &mut QueryBuilder<sqlx::Postgres>,
    table: SearchTable,
    chunk: bool,
) {
    query.push("SELECT ");
    query.push(if chunk {
        "jsonb_build_object('index', c.idx, 'total', (SELECT count(*) FROM rh_message_chunks all_chunks WHERE all_chunks.message_id = c.message_id), 'charStart', c.char_start, 'charEnd', c.char_end, 'content', '') AS chunk, "
    } else {
        "NULL::jsonb AS chunk, "
    });
    if table == SearchTable::Passage {
        query.push("p.id::text, s.id::text AS record_id, p.ordinal AS passage_ordinal, '' AS content_key, ");
    } else {
        let alias = table.alias();
        query.push(format!("{alias}.id::text, NULL::text AS record_id, NULL::int4 AS passage_ordinal, md5({alias}.content) AS content_key, "));
    }
    if table.is_message() {
        query.push(
            "(left(m.content, 10) = '[thinking]') AS reasoning, \
             m.role AS role_or_kind, m.agent, m.tool_name, \
             m.conversation_id::text AS conversation_id, \
             (SELECT conv.session_key FROM rh_conversations conv WHERE conv.id = m.conversation_id) AS session_id, \
             (SELECT conv.channel FROM rh_conversations conv WHERE conv.id = m.conversation_id) AS session_channel, \
             m.created_at, ",
        );
    } else {
        query.push(
            "false AS reasoning, \
             s.kind AS role_or_kind, NULL AS agent, \
             NULL AS tool_name, s.conversation_id::text AS conversation_id, \
             (SELECT conv.session_key FROM rh_conversations conv WHERE conv.id = s.conversation_id) AS session_id, \
             (SELECT conv.channel FROM rh_conversations conv WHERE conv.id = s.conversation_id) AS session_channel, \
",
        );
        query.push(format!("{} AS created_at, ", table.time()));
    }
}

pub(crate) fn push_embedded_semantic(
    query: &mut QueryBuilder<sqlx::Postgres>,
    distance_expr: &str,
    alias: &str,
    vector: &HalfVector,
) {
    query.push("coalesce(1.0 - (");
    query.push(distance_expr);
    query.push(" <=> ");
    query.push_bind(vector.clone());
    query.push("), ");
    query.push(semantic_floor(alias));
    query.push(")::float8 AS semantic, ");
}
