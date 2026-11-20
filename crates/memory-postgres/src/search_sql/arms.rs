use super::*;

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
