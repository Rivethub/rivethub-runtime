use super::*;

pub(super) struct Run<'a> {
    pub(super) env: &'a Publisher<'a>,
    pub(super) id: Uuid,
    pub(super) request: &'a LiveRequest,
    pub(super) revision: i64,
    pub(super) limits: Limits,
    pub(super) meter: Meter,
    pub(super) profile: [u8; 32],
}

pub(super) fn transcript_warnings(input: &SummaryInput) -> BTreeSet<Uuid> {
    input
        .messages
        .iter()
        .filter_map(|row| {
            let metadata = row.metadata.as_ref()?;
            if metadata.get_bool("truncated") != Some(true)
                && metadata.get("full_tool_result_length").is_none()
            {
                return None;
            }
            match row.id.as_deref()?.parse::<Uuid>() {
                Ok(id) => Some(id),
                Err(error) => {
                    tracing::debug!(%error, "ignoring input warning with an invalid message ID");
                    None
                }
            }
        })
        .collect()
}

pub(super) async fn populate(
    run: &Run<'_>,
    tree: &mut PreparedSummaryTree,
    indexes: &mut SummaryIndexes,
    frozen: Frozen,
    mut parts: Vec<PreparedSummaryRecord>,
    warnings: &BTreeSet<Uuid>,
) -> Result<(), WorkerError> {
    let root = Uuid::new_v4();
    let key = run
        .env
        .summary
        .store
        .live_index_key()
        .ok_or_else(|| settings_error("summary embedding specification is unavailable"))?;
    let mut root_flags = if warnings.is_empty() {
        0
    } else {
        INPUT_WARNING
    };
    let (root_key, root_reply) = if frozen.direct {
        let chunk = frozen.chunks.into_iter().next().ok_or(WorkerError::Count)?;
        let prompt = prompt::build(
            Mode::Session {
                total_tokens: frozen.total_tokens,
            },
            chunk.text,
            run.limits,
        );
        run.call(prompt, &frozen.sources).await?
    } else {
        summarize_parts(
            run,
            root,
            &mut parts,
            frozen.chunks,
            frozen.issued_after,
            warnings,
        )
        .await?;
        let current: BTreeMap<_, _> = frozen
            .sources
            .iter()
            .map(|source| (source.id, source))
            .collect();
        for part in &mut parts {
            part.parent = Some(root);
        }
        parts.sort_by_key(|part| {
            let positions = part
                .sources
                .iter()
                .filter_map(|entry| current.get(&entry.message).map(|source| source.ordinal));
            (
                positions.clone().max(),
                positions.min(),
                part.issued_part_ordinal,
            )
        });
        tree.part_order = parts.iter().map(|part| part.id).collect();
        let rebased = parts
            .iter()
            .map(|part| rebase(part, &current))
            .collect::<Result<Vec<_>, _>>()?;
        root_flags |= if parts.iter().any(|part| part.quality_flags != 0) {
            INPUT_WARNING
        } else {
            0
        };
        let (composition, groups) =
            compose_input(&rebased, &parts, frozen.total_tokens, run.meter, run.limits)?;
        run.compose(composition, &frozen.sources, &groups).await?
    };
    let mut root_reply = root_reply;
    root_reply.quality_flags |= root_flags;
    let root = record(
        root,
        SummaryKind::Session,
        root_key,
        &root_reply,
        &frozen.sources,
        &run.env.summary.settings.model,
    )?;
    parts.push(root);
    index_records(run, tree, indexes, key, parts, &frozen.sources).await
}

async fn summarize_parts(
    run: &Run<'_>,
    root: Uuid,
    parts: &mut Vec<PreparedSummaryRecord>,
    chunks: Vec<Chunk>,
    issued_after: i32,
    warnings: &BTreeSet<Uuid>,
) -> Result<(), WorkerError> {
    let issued_after = usize::try_from(issued_after).map_err(|_| WorkerError::Count)?;
    let count = chunks.len();
    let mut jobs = stream::iter(chunks.into_iter().enumerate().map(|(position, chunk)| {
        let issued = issued_after + position + 1;
        async move {
            let sources = chunk_sources(&chunk);
            let prompt = prompt::build(
                Mode::Part {
                    issued,
                    last: position + 1 == count,
                },
                chunk.text,
                run.limits,
            );
            let (key, mut reply) = run.call(prompt, &sources).await?;
            if sources.iter().any(|source| warnings.contains(&source.id)) {
                reply.quality_flags |= INPUT_WARNING;
            }
            let mut part = record(
                Uuid::new_v4(),
                SummaryKind::Part,
                key,
                &reply,
                &sources,
                &run.env.summary.settings.model,
            )?;
            part.parent = Some(root);
            part.ordinal = integer(issued)?;
            part.issued_part_ordinal = Some(part.ordinal);
            part.input_tokens =
                Some(u64::try_from(chunk.input_token_estimate).map_err(|_| WorkerError::Count)?);
            Ok::<_, WorkerError>(part)
        }
    }))
    .buffer_unordered(if run.env.conversion.is_some() {
        1
    } else {
        settings::PART_PARALLELISM
    });
    let mut failure = None;
    while let Some(result) = jobs.next().await {
        match result {
            Ok(part) => parts.push(part),
            Err(error) => {
                if run.env.conversion.is_some() {
                    return Err(error);
                }
                failure.get_or_insert(error);
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }

    Ok(())
}

async fn index_records(
    run: &Run<'_>,
    tree: &mut PreparedSummaryTree,
    indexes: &mut SummaryIndexes,
    key: [u8; 32],
    parts: Vec<PreparedSummaryRecord>,
    sources: &[Source],
) -> Result<(), WorkerError> {
    let times: BTreeMap<_, _> = sources
        .iter()
        .map(|source| (source.id, source.created_at))
        .collect();
    for mut row in parts {
        let sources = owner_sources(&row, &times)?;
        let sections = llm::sections(&row.content, &sources);
        if !tree.retained_parts.contains(&row.id) {
            row.index_key = Some(key);
            let index = run.index(&row, &sources, &sections).await?;
            tree.passage_windows.insert(
                row.id,
                index
                    .passages
                    .iter()
                    .map(|piece| piece.chars.clone())
                    .collect(),
            );
            indexes.insert(row.id, index);
        }
        add_sections(&row, &sections, &mut tree.records)?;
        tree.records.push(row);
    }
    Ok(())
}

type ComposeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<([u8; 32], Reply), WorkerError>> + Send + 'a>>;

impl Run<'_> {
    async fn call(
        &self,
        prompt: Prompt,
        sources: &[Source],
    ) -> Result<([u8; 32], Reply), WorkerError> {
        let store = self.env.summary.store;
        let key = digest(&(
            self.profile,
            &prompt.system,
            &prompt.user,
            prompt.max_tokens,
            sources,
        ))?;
        if let Some(saved) = store.live_checkpoint(self.id, key).await? {
            return Ok((key, decode(&saved)?));
        }
        let (local, permit) = self
            .env
            .model_slot(self.request.conversation_id, self.revision)
            .await?;
        let provider = self.env.summary.provider;
        let model = &self.env.summary.settings.model;
        let tokens = self
            .count(&prompt.system)
            .await?
            .saturating_add(self.count(&prompt.user).await?)
            .saturating_add(CHAT_FRAMING_TOKENS + prompt.max_tokens as usize);
        if tokens > self.limits.context_tokens() {
            return Err(settings_error("summary request exceeds the model context"));
        }
        ensure_eligible(self.env, self.request.conversation_id, self.revision).await?;
        let charge = self
            .env
            .conversion
            .map(|_| -> Result<_, WorkerError> {
                Ok(ConversionCharge {
                    estimate: i64::try_from(tokens - prompt.max_tokens as usize)
                        .map_err(|_| WorkerError::Count)?,
                    capacity: i64::try_from(self.limits.context_tokens())
                        .map_err(|_| WorkerError::Count)?,
                })
            })
            .transpose()?;
        self.env.boundary(charge).await?;
        let response = self
            .env
            .request(provider.chat(llm::model_request(model, &prompt)))
            .await?;
        drop(permit);
        drop(local);
        if let (Some((conversion, _)), Some(charge), Some(usage)) =
            (self.env.conversion, charge, &response.usage)
        {
            conversion
                .report(ConversionUsage {
                    estimate: charge.estimate,
                    input: i64::try_from(usage.prompt_tokens).map_err(|_| WorkerError::Count)?,
                    output: i64::try_from(usage.completion_tokens)
                        .map_err(|_| WorkerError::Count)?,
                })
                .await?;
        }
        let reply = llm::parse_model_reply(&response, prompt.editorial_ceiling, sources)
            .map_err(|_| WorkerError::EmptySummary)?;
        store
            .save_live_checkpoint(self.id, self.revision, key, encode(&reply)?)
            .await?;
        Ok((key, reply))
    }

    async fn index(
        &self,
        row: &PreparedSummaryRecord,
        sources: &[Source],
        sections: &[llm::Section],
    ) -> Result<SummaryIndex, WorkerError> {
        let key = digest(&(
            "summary-index-v2",
            row.index_key,
            &row.content,
            sources,
            sections,
        ))?;
        let store = self.env.summary.store;
        if let Some(saved) = store.live_checkpoint(self.id, key).await? {
            return decode(&saved);
        }
        ensure_eligible(self.env, self.request.conversation_id, self.revision).await?;
        let coverage = sources
            .iter()
            .map(|source| (source.ordinal, source.created_at))
            .collect::<Vec<_>>();
        let ranges = sections
            .iter()
            .map(|section| {
                (
                    section.chars.clone(),
                    section.source_first..=section.source_last,
                )
            })
            .collect::<Vec<_>>();
        let index = prepare_index(
            self.env.embedder,
            self.env.embed,
            &row.content,
            &coverage,
            &ranges,
        )
        .await?;
        store
            .save_live_checkpoint(self.id, self.revision, key, encode(&index)?)
            .await?;
        Ok(index)
    }

    async fn count(&self, text: &str) -> Result<usize, WorkerError> {
        Ok(
            tokenize(self.env, self.request.conversation_id, self.revision, text)
                .await?
                .unwrap_or_else(|| self.meter.estimate(text)),
        )
    }

    fn compose<'a>(
        &'a self,
        plan: ComposePlan,
        current: &'a [Source],
        parts: &'a [&'a PreparedSummaryRecord],
    ) -> ComposeFuture<'a> {
        Box::pin(async move {
            let members: BTreeSet<_> = parts
                .iter()
                .flat_map(|part| part.sources.iter().map(|entry| entry.message))
                .collect();
            let sources: Vec<_> = current
                .iter()
                .filter(|source| members.contains(&source.id))
                .cloned()
                .collect();
            match plan {
                ComposePlan::Call(prompt) => self.call(prompt, &sources).await,
                ComposePlan::Reduce { groups, parent } => {
                    let [left, right] = *groups;
                    let (before, after) = parts.split_at(parts.len() / 2);
                    let (_, left) = self.compose(left, current, before).await?;
                    let (_, right) = self.compose(right, current, after).await?;
                    let (key, mut reply) = self
                        .call(
                            plan::after_reduction(parent, [&left.text, &right.text]),
                            &sources,
                        )
                        .await?;
                    if left.quality_flags != 0 || right.quality_flags != 0 {
                        reply.quality_flags |= INPUT_WARNING;
                    }
                    Ok((key, reply))
                }
            }
        })
    }
}

fn compose_input<'a>(
    records: &[String],
    parts: &'a [PreparedSummaryRecord],
    total_tokens: usize,
    meter: Meter,
    limits: Limits,
) -> Result<(ComposePlan, Vec<&'a PreparedSummaryRecord>), WorkerError> {
    match plan::compose(records, total_tokens, meter, limits) {
        Ok(plan) => return Ok((plan, parts.iter().collect())),
        Err(PlanError::ComposeContext) => {}
        Err(error) => return Err(settings_error(error)),
    }
    let budget = limits
        .context_tokens()
        .saturating_sub(2 * settings::CARRY_CEILING as usize + 2_032);
    let mut split = Vec::new();
    let mut owners = Vec::new();
    for (text, part) in records.iter().zip(parts) {
        let mut remaining = vec![text.as_str()];
        while let Some(piece) = remaining.pop() {
            if meter.estimate(piece) < budget {
                split.push(piece);
                owners.push(part);
                continue;
            }
            let middle = piece
                .char_indices()
                .nth(piece.chars().count() / 2)
                .map(|(byte, _)| byte)
                .filter(|&byte| byte > 0)
                .ok_or_else(|| settings_error(PlanError::ComposeContext))?;
            let (left, right) = piece.split_at(middle);
            remaining.push(right);
            remaining.push(left);
        }
    }
    let plan = plan::compose(&split, total_tokens, meter, limits).map_err(settings_error)?;
    Ok((plan, owners))
}

fn chunk_sources(chunk: &Chunk) -> Vec<Source> {
    let mut sources: Vec<_> = chunk
        .fragments
        .iter()
        .map(|fragment| fragment.source.clone())
        .collect();
    sources.dedup_by_key(|source| source.ordinal);
    sources
}

fn owner_sources(
    row: &PreparedSummaryRecord,
    times: &BTreeMap<Uuid, Timestamp>,
) -> Result<Vec<Source>, WorkerError> {
    row.sources
        .iter()
        .map(|entry| {
            Ok(Source {
                id: entry.message,
                ordinal: usize::try_from(entry.ordinal).map_err(|_| WorkerError::Count)?,
                created_at: *times.get(&entry.message).ok_or(WorkerError::Count)?,
                render_key: entry.render_key.ok_or(WorkerError::Count)?,
            })
        })
        .collect()
}

fn rebase(
    part: &PreparedSummaryRecord,
    current: &BTreeMap<Uuid, &Source>,
) -> Result<String, WorkerError> {
    let times = current
        .iter()
        .map(|(&id, source)| (id, source.created_at))
        .collect();
    let sources = owner_sources(part, &times)?;
    let sections = llm::sections(&part.content, &sources);
    let mut result = String::new();
    for section in sections {
        use std::fmt::Write as _;
        let text: String = part
            .content
            .chars()
            .skip(section.chars.start)
            .take(section.chars.len())
            .collect();
        let Some((heading, body)) = text
            .split_once('\n')
            .or_else(|| text.starts_with("## ").then_some((text.as_str(), "")))
        else {
            result.push_str(&text);
            continue;
        };
        if !heading.starts_with("## ") {
            result.push_str(&text);
            continue;
        }
        let title = heading
            .rsplit_once(" [m")
            .map_or(heading, |(title, _)| title);
        result.push_str(title);
        if section.range_verified {
            let mapped = |ordinal| {
                sources
                    .iter()
                    .find(|source| source.ordinal == ordinal)
                    .and_then(|source| current.get(&source.id))
                    .map(|source| source.ordinal)
            };
            if let (Some(first), Some(last)) =
                (mapped(section.source_first), mapped(section.source_last))
                && first <= last
            {
                write!(result, " [{}-{}]", plan::tag(first), plan::tag(last))
                    .map_err(settings_error)?;
            }
        }
        if text.contains('\n') {
            result.push('\n');
            result.push_str(body);
        }
    }
    Ok(result)
}

fn record(
    id: Uuid,
    kind: SummaryKind,
    input_key: [u8; 32],
    reply: &Reply,
    sources: &[Source],
    model: &str,
) -> Result<PreparedSummaryRecord, WorkerError> {
    let sources = sources
        .iter()
        .map(|source| {
            Ok(SummaryManifestEntry {
                message: source.id,
                ordinal: integer(source.ordinal)?,
                render_key: Some(source.render_key),
            })
        })
        .collect::<Result<_, WorkerError>>()?;
    Ok(PreparedSummaryRecord {
        id,
        parent: None,
        kind,
        ordinal: 0,
        content: reply.text.clone(),
        input_key,
        index_key: None,
        model: Some(model.to_owned()),
        issued_part_ordinal: None,
        input_tokens: None,
        quality_flags: i32::try_from(reply.quality_flags).map_err(|_| WorkerError::Count)?,
        section: None,
        sources,
    })
}

fn add_sections(
    owner: &PreparedSummaryRecord,
    sections: &[llm::Section],
    rows: &mut Vec<PreparedSummaryRecord>,
) -> Result<(), WorkerError> {
    let chars: Vec<_> = owner.content.chars().collect();
    for (ordinal, section) in sections.iter().enumerate() {
        rows.push(PreparedSummaryRecord {
            id: Uuid::new_v4(),
            parent: Some(owner.id),
            kind: SummaryKind::Section,
            ordinal: integer(ordinal)?,
            content: chars
                .get(section.chars.clone())
                .ok_or(WorkerError::Count)?
                .iter()
                .collect(),
            input_key: owner.input_key,
            index_key: None,
            model: owner.model.clone(),
            issued_part_ordinal: None,
            input_tokens: None,
            quality_flags: owner.quality_flags,
            section: Some(SummarySectionBounds {
                first: integer(section.source_first)?,
                last: integer(section.source_last)?,
                verified: section.range_verified,
                char_start: integer(section.chars.start)?,
                char_end: integer(section.chars.end)?,
            }),
            sources: Vec::new(),
        });
    }
    Ok(())
}
