use crate::error::Error;
use crate::search::ArmKind;
use crate::search::ArmRow;
use crate::search::TagFilter;
use crate::search::{SearchOptions, SearchRead};
use crate::search_literal::Patience;
use crate::search_literal::literal_tokens;
use jiff::Timestamp;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory_core::hybrid_pool;
use kernel::memory_core::recency_boost;
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
            Self::Legacy => " AND s.kind IN ('leaf', 'branch', 'root')",
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

pub(crate) struct ArmContext<'a> {
    pub(crate) options: &'a SearchOptions,
    pub(crate) tag_filter: Option<&'a TagFilter>,
    pub(crate) patience: Patience,
}

impl SearchRead<'_> {
    pub(crate) async fn arm_rows(
        &mut self,
        options: &SearchOptions,
        kind: ArmKind,
        tag_filter: Option<&TagFilter>,
        query_vector: Option<&[f32]>,
    ) -> Result<Vec<ArmRow>, Error> {
        self.arm_rows_with(options, kind, tag_filter, query_vector, Patience::Asked)
            .await
    }

    pub(crate) async fn optional_literal_rows(
        &mut self,
        options: &SearchOptions,
        tag_filter: Option<&TagFilter>,
        query_vector: Option<&[f32]>,
    ) -> Result<Option<Vec<ArmRow>>, Error> {
        match self
            .arm_rows_with(
                options,
                ArmKind::Trigram,
                tag_filter,
                query_vector,
                Patience::Optional,
            )
            .await
        {
            Ok(rows) => Ok(Some(rows)),
            Err(error) if error.timed_out() => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn arm_rows_with(
        &mut self,
        options: &SearchOptions,
        kind: ArmKind,
        tag_filter: Option<&TagFilter>,
        query_vector: Option<&[f32]>,
        patience: Patience,
    ) -> Result<Vec<ArmRow>, Error> {
        let context = ArmContext {
            options,
            tag_filter,
            patience,
        };
        if !matches!(kind, ArmKind::Trigram | ArmKind::Regex) {
            return Self::lexical_rows(&mut self.tx, &context, kind, query_vector).await;
        }
        let mut savepoint = self
            .tx
            .begin()
            .await
            .map_err(|source| Error::operation("search savepoint", source))?;
        sqlx::query("select set_config('statement_timeout', $1, true)")
            .bind(patience.statement_timeout())
            .execute(&mut *savepoint)
            .await
            .map_err(|source| Error::operation("search", source))?;
        let found = Self::lexical_rows(&mut savepoint, &context, kind, query_vector).await;
        savepoint
            .rollback()
            .await
            .map_err(|source| Error::operation("search savepoint", source))?;
        found
    }

    async fn lexical_rows(
        tx: &mut sqlx::PgConnection,
        context: &ArmContext<'_>,
        kind: ArmKind,
        query_vector: Option<&[f32]>,
    ) -> Result<Vec<ArmRow>, Error> {
        use SearchTable::{Legacy, Message, Passage};
        let text_search = matches!(kind, ArmKind::Fts | ArmKind::FtsExplicit);
        let options = context.options;
        let pool = hybrid_pool(options.limit);
        let mut rows = if matches!(options.scope, MemoryScope::Messages | MemoryScope::Both) {
            let candidates = if text_search {
                crate::search_cap::capped_candidates(tx, context, &[Message], kind == ArmKind::Fts)
                    .await?
            } else {
                None
            };
            Self::arm_table(
                tx,
                context,
                kind,
                Message,
                query_vector,
                candidates.as_deref(),
            )
            .await?
        } else {
            Vec::new()
        };
        if matches!(options.scope, MemoryScope::Summaries | MemoryScope::Both) {
            let candidates = if text_search {
                crate::search_cap::capped_candidates(
                    tx,
                    context,
                    &[Legacy, Passage],
                    kind == ArmKind::Fts,
                )
                .await?
            } else {
                None
            };
            let mut summaries = Self::arm_table(
                tx,
                context,
                kind,
                Legacy,
                query_vector,
                candidates.as_deref(),
            )
            .await?;
            let mut passages = Self::arm_table(
                tx,
                context,
                kind,
                Passage,
                query_vector,
                candidates.as_deref(),
            )
            .await?;
            summaries.append(&mut passages);
            sort_lexical(&mut summaries, kind, candidates.is_some());
            summaries.truncate(usize::try_from(pool)?);
            rows.append(&mut summaries);
        }
        sort_lexical(&mut rows, kind, false);
        Ok(rows)
    }

    #[expect(clippy::too_many_lines)]
    async fn arm_table(
        tx: &mut sqlx::PgConnection,
        context: &ArmContext<'_>,
        kind: ArmKind,
        table: SearchTable,
        query_vector: Option<&[f32]>,
        candidates: Option<&[uuid::Uuid]>,
    ) -> Result<Vec<ArmRow>, Error> {
        let options = context.options;
        let tag_filter = context.tag_filter;
        let pool = hybrid_pool(options.limit);
        let is_message = table.is_message();
        let alias = table.alias();
        let literal = LiteralText::of(options, table);
        let tokens = if kind == ArmKind::Trigram {
            literal_tokens(&options.text, context.patience)
        } else {
            Vec::new()
        };
        if kind == ArmKind::Trigram && tokens.is_empty() {
            return Ok(Vec::new());
        }
        if kind == ArmKind::Trigram
            && is_message
            && let Some(rows) =
                crate::search_recent::recent_literals(tx, context, &tokens, query_vector, pool)
                    .await?
        {
            return Ok(rows);
        }
        let text_search = matches!(kind, ArmKind::Fts | ArmKind::FtsExplicit);
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, table, false);
        query.push(format!("{} AS newest, ", table.newest()));
        match kind {
            ArmKind::Fts | ArmKind::FtsExplicit => {
                query.push(format!(
                    "ts_rank_cd({alias}.content_tsv, plainto_tsquery('english', "
                ));
                query.push_bind(options.text.clone());
                query.push(if is_message {
                    "), 32)::float8"
                } else {
                    "))::float8"
                });
            }
            ArmKind::Trigram => push_literal_score(&mut query, &tokens, literal),
            ArmKind::Regex => {
                query.push("1.0::float8");
            }
        }
        query.push(" AS score, ");
        if let Some(vector) = query_vector.map(HalfVector::from_f32_slice) {
            let distance = format!("{alias}.embedding");
            push_embedded_semantic(&mut query, &distance, alias, &vector);
        } else {
            query.push(semantic_floor(alias));
            query.push("::float8 AS semantic, ");
        }
        query.push(format!("{} FROM {}", recency_columns(table), table.from()));
        if text_search {
            crate::search_fts::push_fts_where(&mut query, context, table, kind == ArmKind::Fts);
            if let Some(candidates) = candidates {
                query.push(format!(" AND {alias}.id = ANY("));
                query.push_bind(candidates);
                query.push(")");
            }
        } else {
            query.push(" WHERE ");
            if let Some(filter) = tag_filter {
                filter.push_condition(&mut query, is_message);
            } else {
                query.push("true");
            }
            match kind {
                ArmKind::Trigram => push_literal_match(&mut query, &tokens, literal),
                ArmKind::Regex => push_pattern_match(&mut query, &options.text, literal),
                ArmKind::Fts | ArmKind::FtsExplicit => {}
            }
            table.push_eligibility(&mut query);
            push_time_conditions(&mut query, options, table);
            push_scope_conditions(&mut query, options, is_message);
        }
        query.push(" ORDER BY ");
        if kind == ArmKind::FtsExplicit && is_message {
            query.push("reasoning, ");
        }
        if candidates.is_some() {
            query.push(format!("score DESC, {} DESC LIMIT {pool}", table.newest()));
        } else if kind == ArmKind::Trigram {
            query.push(format!("score DESC, {} DESC LIMIT {pool}", table.time()));
        } else {
            query.push(format!("score DESC LIMIT {pool}"));
        }
        let fetched = query
            .build_query_as::<ArmSqlRow>()
            .fetch_all(tx)
            .await
            .map_err(|source| Error::operation("search", source))?;
        fetched
            .into_iter()
            .map(|row| row.into_arm(is_message))
            .collect()
    }
}

fn sort_lexical(rows: &mut [ArmRow], kind: ArmKind, capped: bool) {
    rows.sort_by(|a, b| {
        let by_score = b.arm_score.total_cmp(&a.arm_score);
        if kind == ArmKind::Trigram {
            by_score.then_with(|| b.created_at.cmp(&a.created_at))
        } else if capped {
            by_score.then_with(|| match (a.newest, b.newest) {
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (a, b) => b.cmp(&a),
            })
        } else {
            by_score
        }
    });
}
