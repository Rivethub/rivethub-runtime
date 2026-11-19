mod compose;

use super::*;
use compose::{compose_input, rebase};

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
        let current: BTreeMap<_, _> = frozen
            .sources
            .iter()
            .map(|source| (source.id, source))
            .collect();
        let prompt_positions = part_prompt_positions(&parts, &frozen.chunks, &current);
        summarize_parts(
            run,
            root,
            &mut parts,
            frozen.chunks.into_iter().zip(prompt_positions),
            frozen.issued_after,
            warnings,
        )
        .await?;
        for part in &mut parts {
            part.parent = Some(root);
        }
        parts.sort_by_key(|part| (part_position(part, &current), part.issued_part_ordinal));
        tree.part_order = parts.iter().map(|part| part.id).collect();
        let rebased = parts
            .iter()
            .map(|part| rebase(part, &current, tree.retained_parts.contains(&part.id)))
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

fn part_position(
    part: &PreparedSummaryRecord,
    current: &BTreeMap<Uuid, &Source>,
) -> (Option<usize>, Option<usize>) {
    let positions = part
        .sources
        .iter()
        .filter_map(|entry| current.get(&entry.message).map(|source| source.ordinal));
    (positions.clone().max(), positions.min())
}

fn part_prompt_positions(
    kept: &[PreparedSummaryRecord],
    chunks: &[Chunk],
    current: &BTreeMap<Uuid, &Source>,
) -> Vec<(usize, bool)> {
    if kept.is_empty() {
        return (1..=chunks.len())
            .map(|number| (number, number == chunks.len()))
            .collect();
    }
    let mut positions: Vec<_> = kept
        .iter()
        .map(|part| (part_position(part, current), None))
        .collect();
    positions.extend(chunks.iter().enumerate().map(|(index, chunk)| {
        let positions = chunk
            .fragments
            .iter()
            .map(|fragment| fragment.source.ordinal);
        ((positions.clone().max(), positions.min()), Some(index))
    }));
    positions.sort_unstable();
    let total = positions.len();
    let mut prompts: Vec<_> = positions
        .into_iter()
        .enumerate()
        .filter_map(|(position, (_, index))| {
            index.map(|index| (index, (position + 1, position + 1 == total)))
        })
        .collect();
    prompts.sort_unstable_by_key(|(index, _)| *index);
    prompts.into_iter().map(|(_, position)| position).collect()
}

async fn summarize_parts(
    run: &Run<'_>,
    root: Uuid,
    parts: &mut Vec<PreparedSummaryRecord>,
    chunks: impl Iterator<Item = (Chunk, (usize, bool))>,
    issued_after: i32,
    warnings: &BTreeSet<Uuid>,
) -> Result<(), WorkerError> {
    let issued_after = usize::try_from(issued_after).map_err(|_| WorkerError::Count)?;
    let mut jobs = stream::iter(
        chunks
            .enumerate()
            .map(|(position, (chunk, (number, last)))| {
                let issued = issued_after + position + 1;
                async move {
                    let sources = chunk_sources(&chunk);
                    let prompt = prompt::build(
                        Mode::Part {
                            issued: number,
                            last,
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
                    part.input_tokens = Some(
                        u64::try_from(chunk.input_token_estimate)
                            .map_err(|_| WorkerError::Count)?,
                    );
                    Ok::<_, WorkerError>(part)
                }
            }),
    )
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
            .chat_request(llm::model_request(model, &prompt))
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
                .filter(|&count| count != 0 || text.is_empty())
                .unwrap_or_else(|| self.meter.estimate(text)),
        )
    }
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

#[cfg(test)]
mod regressions {
    use super::*;
    use crate::summarize::live::packing::retained_coverage;

    fn source(ordinal: usize) -> Source {
        Source {
            id: Uuid::from_u128(u128::from(u64::try_from(ordinal).expect("ordinal"))),
            ordinal,
            created_at: Timestamp::UNIX_EPOCH,
            render_key: [1; 32],
        }
    }

    fn stored_part(id: u128, text: &str, sources: &[Source]) -> PreparedSummaryRecord {
        let reply = Reply {
            text: text.into(),
            sections: Vec::new(),
            quality_flags: 0,
        };
        record(
            Uuid::from_u128(id),
            SummaryKind::Part,
            [0; 32],
            &reply,
            sources,
            "fixture",
        )
        .expect("stored part")
    }

    #[test]
    fn reopened_parts_invalidate_all_owners_of_split_messages() {
        let sources = [1, 2, 3, 4].map(source);
        let parts = [
            stored_part(11, "one", &sources[..2]),
            stored_part(12, "two", &sources[1..3]),
            stored_part(13, "three", &sources[2..3]),
            stored_part(14, "independent", &sources[3..]),
        ];
        let mut current = sources;
        current[0].render_key = [2; 32];
        let retained = retained_coverage(current.iter(), &parts);
        assert_eq!(retained.parts, BTreeSet::from([parts[3].id]));
        assert_eq!(retained.messages, BTreeSet::from([current[3].id]));
    }

    #[test]
    fn rebased_headings_preserve_range_like_title_text() {
        let mut current = source(1);
        let part = stored_part(
            2,
            "## Replace [marker]\nDetails\n## Verify [marker] [m00001-m00001]\nMore",
            std::slice::from_ref(&current),
        );
        current.ordinal = 5;
        assert_eq!(
            rebase(&part, &BTreeMap::from([(current.id, &current)]), true).expect("rebase"),
            "## Replace [marker]\nDetails\n## Verify [marker] [m00005-m00005]\nMore"
        );
    }
}
