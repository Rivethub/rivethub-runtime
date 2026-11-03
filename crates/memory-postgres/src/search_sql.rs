use crate::error::Error;
use crate::search::ArmKind;
use crate::search::ArmRow;
use crate::search::SearchOptions;
use crate::search::TagFilter;
use chrono::Utc;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory_core::HYBRID_MIN_CONTENT_LEN;
use kernel::memory_core::hybrid_pool;
use kernel::memory_core::recency_boost;
use kernel::memory_core::should_trigram_fallback;
use pgvector::HalfVector;
use sqlx::QueryBuilder;
use std::str::FromStr as _;
use wire::message::Role;

pub(crate) fn semantic_floor(alias: &str) -> String {
    format!("least(length(btrim({alias}.content)) / 1000.0, 1.0)")
}

const LENGTH_PREFIX: i64 = 1000;

fn long_enough(body: &str) -> String {
    format!(
        "(length(btrim(left({body}, {LENGTH_PREFIX}))) >= {HYBRID_MIN_CONTENT_LEN} \
         OR (octet_length({body}) > {LENGTH_PREFIX} AND length(btrim({body})) >= {HYBRID_MIN_CONTENT_LEN}))"
    )
}

pub(crate) fn recency_columns(alias: &str, is_message: bool) -> String {
    let (importance, access_table, access_key) = if is_message {
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
        "(EXTRACT(EPOCH FROM (now() - {alias}.created_at)) / 86400.0)::float8 AS age_days, \
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
    alias: &str,
) {
    if let Some(since) = options.since {
        query.push(format!(" AND {alias}.created_at >= "));
        query.push_bind(since);
    }
    if let Some(before) = options.before {
        query.push(format!(" AND {alias}.created_at < "));
        query.push_bind(before);
    }
}

pub(crate) fn push_scope_conditions(
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

fn literal_tokens(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for token in text.split_whitespace() {
        if should_trigram_fallback(token) && !tokens.iter().any(|seen| seen == token) {
            tokens.push(token.to_owned());
        }
    }
    let whole = text.trim();
    if tokens.is_empty() && !whole.is_empty() {
        tokens.push(whole.to_owned());
    }
    tokens
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
    Message,
    MessageAndResult,
}

impl LiteralText {
    pub(crate) fn of(options: &SearchOptions, is_message: bool) -> Self {
        match (is_message, options.admits(Role::Tool)) {
            (false, _) => Self::Summary,
            (true, true) => Self::MessageAndResult,
            (true, false) => Self::Message,
        }
    }

    const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Summary => &["s.content"],
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
    if text == LiteralText::Summary {
        query.push(" AND ");
        push_any_contains(query, "s.content", tokens);
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

pub(crate) fn push_fts_where(
    query: &mut QueryBuilder<sqlx::Postgres>,
    context: &ArmContext<'_>,
    is_message: bool,
    long: bool,
) {
    let alias = if is_message { "m" } else { "s" };
    query.push(" WHERE ");
    if let Some(filter) = context.tag_filter {
        filter.push_condition(query, is_message);
    } else {
        query.push("true");
    }
    query.push(format!(
        " AND {alias}.content_tsv @@ plainto_tsquery('english', "
    ));
    query.push_bind(context.options.text.clone());
    query.push(")");
    if long {
        query.push(" AND ");
        query.push(long_enough(if is_message {
            "CASE WHEN m.role = 'tool' THEN coalesce(m.tool_result, '') ELSE m.content END"
        } else {
            "s.content"
        }));
    }
    push_time_conditions(query, context.options, alias);
    push_scope_conditions(query, context.options, is_message);
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct ArmSqlRow {
    chunk: Option<sqlx::types::Json<kernel::memory::MemoryChunkMatch>>,
    id: String,
    content_key: String,
    reasoning: bool,
    role_or_kind: Option<String>,
    agent: Option<String>,
    tool_name: Option<String>,
    conversation_id: Option<String>,
    session_id: Option<String>,
    session_channel: Option<String>,
    created_at: chrono::DateTime<Utc>,
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
            id: self.id,
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
            arm_score: self.score,
            semantic: self.semantic,
            boost: recency_boost(self.age_days, self.access_count, self.importance),
        })
    }
}

pub(crate) fn push_select_prefix(
    query: &mut QueryBuilder<sqlx::Postgres>,
    is_message: bool,
    chunk: bool,
) {
    query.push("SELECT ");
    query.push(if chunk {
        "jsonb_build_object('index', c.idx, 'total', (SELECT count(*) FROM rh_message_chunks all_chunks WHERE all_chunks.message_id = c.message_id), 'charStart', c.char_start, 'charEnd', c.char_end, 'content', '') AS chunk, "
    } else {
        "NULL::jsonb AS chunk, "
    });
    if is_message {
        query.push(
            "m.id::text, md5(m.content) AS content_key, \
             (left(m.content, 10) = '[thinking]') AS reasoning, \
             m.role AS role_or_kind, m.agent, m.tool_name, \
             m.conversation_id::text AS conversation_id, \
             (SELECT conv.session_key FROM rh_conversations conv WHERE conv.id = m.conversation_id) AS session_id, \
             (SELECT conv.channel FROM rh_conversations conv WHERE conv.id = m.conversation_id) AS session_channel, \
             m.created_at, ",
        );
    } else {
        query.push(
            "s.id::text, md5(s.content) AS content_key, false AS reasoning, \
             s.kind AS role_or_kind, NULL AS agent, \
             NULL AS tool_name, s.conversation_id::text AS conversation_id, \
             (SELECT conv.session_key FROM rh_conversations conv WHERE conv.id = s.conversation_id) AS session_id, \
             (SELECT conv.channel FROM rh_conversations conv WHERE conv.id = s.conversation_id) AS session_channel, \
             s.created_at, ",
        );
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
}

const ESCAPE_HATCH_TIMEOUT: &str = "30s";

impl crate::PostgresMemory {
    pub(crate) async fn arm_rows(
        &self,
        options: &SearchOptions,
        kind: ArmKind,
        tag_filter: Option<&TagFilter>,
        query_vector: Option<&[f32]>,
    ) -> Result<Vec<ArmRow>, Error> {
        let context = ArmContext {
            options,
            tag_filter,
        };
        let messages = async {
            if matches!(options.scope, MemoryScope::Messages | MemoryScope::Both) {
                self.arm_table(&context, kind, true, query_vector).await
            } else {
                Ok(Vec::new())
            }
        };
        let summaries = async {
            if matches!(options.scope, MemoryScope::Summaries | MemoryScope::Both) {
                self.arm_table(&context, kind, false, query_vector).await
            } else {
                Ok(Vec::new())
            }
        };
        let (mut rows, summary_rows) = tokio::try_join!(Box::pin(messages), Box::pin(summaries))?;
        rows.extend(summary_rows);
        rows.sort_by(|a, b| {
            let by_score = b.arm_score.total_cmp(&a.arm_score);
            if kind == ArmKind::Trigram {
                by_score.then_with(|| b.created_at.cmp(&a.created_at))
            } else {
                by_score
            }
        });
        Ok(rows)
    }

    #[expect(clippy::too_many_lines, reason = "one SQL builder covers both tables")]
    async fn arm_table(
        &self,
        context: &ArmContext<'_>,
        kind: ArmKind,
        is_message: bool,
        query_vector: Option<&[f32]>,
    ) -> Result<Vec<ArmRow>, Error> {
        let options = context.options;
        let tag_filter = context.tag_filter;
        let pool = hybrid_pool(options.limit);
        let alias = if is_message { "m" } else { "s" };
        let literal = LiteralText::of(options, is_message);
        let tokens = if kind == ArmKind::Trigram {
            literal_tokens(&options.text)
        } else {
            Vec::new()
        };
        if kind == ArmKind::Trigram && tokens.is_empty() {
            return Ok(Vec::new());
        }
        if kind == ArmKind::Trigram
            && is_message
            && let Some(rows) = self
                .recent_literals(context, &tokens, query_vector, pool)
                .await?
        {
            return Ok(rows);
        }
        let text_search = matches!(kind, ArmKind::Fts | ArmKind::FtsExplicit);
        let candidates = if text_search {
            self.capped_candidates(context, is_message, kind == ArmKind::Fts)
                .await?
        } else {
            None
        };
        let mut query = QueryBuilder::new("");
        push_select_prefix(&mut query, is_message, false);
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
        query.push(format!("{} FROM ", recency_columns(alias, is_message)));
        query.push(if is_message {
            "rh_messages m"
        } else {
            "rh_summaries s"
        });
        if text_search {
            push_fts_where(&mut query, context, is_message, kind == ArmKind::Fts);
            if let Some(candidates) = &candidates {
                query.push(format!(" AND {alias}.id = ANY("));
                query.push_bind(candidates.clone());
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
            push_time_conditions(&mut query, options, alias);
            push_scope_conditions(&mut query, options, is_message);
        }
        query.push(" ORDER BY ");
        if kind == ArmKind::FtsExplicit && is_message {
            query.push("reasoning, ");
        }
        if candidates.is_some() {
            query.push(format!(
                "score DESC, {} DESC LIMIT {pool}",
                crate::search_cap::newest_column(is_message)
            ));
        } else if kind == ArmKind::Trigram {
            query.push(format!("score DESC, {alias}.created_at DESC LIMIT {pool}"));
        } else {
            query.push(format!("score DESC LIMIT {pool}"));
        }
        let fetched = if matches!(kind, ArmKind::Trigram | ArmKind::Regex) {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|source| Error::operation("search", source))?;
            sqlx::query("select set_config('statement_timeout', $1, true)")
                .bind(ESCAPE_HATCH_TIMEOUT)
                .execute(&mut *tx)
                .await
                .map_err(|source| Error::operation("search", source))?;
            let rows = query
                .build_query_as::<ArmSqlRow>()
                .fetch_all(&mut *tx)
                .await
                .map_err(|source| Error::operation("search", source))?;
            tx.commit()
                .await
                .map_err(|source| Error::operation("search", source))?;
            rows
        } else {
            query
                .build_query_as::<ArmSqlRow>()
                .fetch_all(&self.pool)
                .await
                .map_err(|source| Error::operation("search", source))?
        };
        fetched
            .into_iter()
            .map(|row| row.into_arm(is_message))
            .collect()
    }
}
