use super::*;

#[expect(clippy::too_many_arguments)]
pub(super) async fn freeze(
    env: &Publisher<'_>,
    request: &LiveRequest,
    revision: i64,
    input: &SummaryInput,
    blocks: &[Block],
    sources: Vec<Source>,
    limits: Limits,
    profile: [u8; 32],
) -> Result<Frozen, WorkerError> {
    let id = (request.conversation_id, revision);
    let calibrated = calibrate(env, id, blocks, limits).await?;
    let mut meter = calibrated.unwrap_or_else(Meter::uncalibrated);
    let mut packing = pack_transcript(
        env,
        id,
        blocks,
        meter,
        limits,
        calibrated.is_some(),
        &input.parts,
    )
    .await?;
    if packing.is_none() {
        meter = Meter::uncalibrated();
        packing = pack_transcript(env, id, blocks, meter, limits, false, &input.parts).await?;
    }
    let packed = packing.ok_or(WorkerError::Count)?;
    Ok(Frozen {
        profile,
        root: input.expected_root,
        sources,
        ratio: meter.ratio(),
        total_tokens: packed.total_tokens,
        retained: packed.coverage.parts,
        issued_after: packed.coverage.issued_after,
        direct: packed.direct,
        chunks: packed.chunks,
    })
}

async fn calibrate(
    env: &Publisher<'_>,
    (id, revision): (Uuid, i64),
    blocks: &[Block],
    limits: Limits,
) -> Result<Option<Meter>, WorkerError> {
    let sample: String = blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| {
            (index != 0)
                .then_some('\n')
                .into_iter()
                .chain(block.text.chars())
        })
        .take(200_000)
        .collect();
    if sample.is_empty() {
        return Ok(None);
    }
    let (_local, _permit) = env.model_slot(id, revision).await?;
    let Some(count) = tokenize(env, id, revision, &sample).await? else {
        return Ok(None);
    };
    match Meter::calibrate(&sample, count).and_then(|meter| {
        meter.cut_chars(limits.chunk_tokens())?;
        Ok(meter)
    }) {
        Ok(meter) => Ok(Some(meter)),
        Err(error) => {
            tracing::warn!(conversation = %id, revision, count, %error, "invalid summary tokenizer calibration; using character estimates");
            Ok(None)
        }
    }
}

struct PackedTranscript {
    total_tokens: usize,
    coverage: RetainedCoverage,
    chunks: Vec<Chunk>,
    direct: bool,
}

async fn pack_transcript(
    env: &Publisher<'_>,
    id: (Uuid, i64),
    blocks: &[Block],
    meter: Meter,
    limits: Limits,
    exact: bool,
    parts: &[PreparedSummaryRecord],
) -> Result<Option<PackedTranscript>, WorkerError> {
    let Some(all) = pack(env, id, blocks, meter, limits, exact).await? else {
        return Ok(None);
    };
    let direct = all.chunks.len() <= 1;
    let mut coverage = RetainedCoverage::default();
    let mut chunks = all.chunks;
    if !direct && !parts.is_empty() {
        coverage = retained_coverage(blocks.iter().map(|block| &block.source), parts);
        if !coverage.parts.is_empty() {
            chunks.clear();
            for uncovered in blocks.split(|block| coverage.messages.contains(&block.source.id)) {
                let Some(packed) = pack(env, id, uncovered, meter, limits, exact).await? else {
                    return Ok(None);
                };
                chunks.extend(packed.chunks);
            }
        }
    }
    Ok(Some(PackedTranscript {
        total_tokens: all.total_tokens,
        coverage,
        chunks,
        direct,
    }))
}

#[derive(Default)]
struct RetainedCoverage {
    parts: BTreeSet<Uuid>,
    messages: BTreeSet<Uuid>,
    issued_after: i32,
}

fn retained_coverage<'a>(
    sources: impl Iterator<Item = &'a Source>,
    parts: &[PreparedSummaryRecord],
) -> RetainedCoverage {
    let mut retained = BTreeSet::new();
    let mut issued_after = 0;
    let current: BTreeMap<_, _> = sources.map(|source| (source.id, source)).collect();
    let mut covered = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    for part in parts {
        if let Some(ordinal) = part.issued_part_ordinal {
            issued_after = issued_after.max(ordinal);
        }
        let mut previous = 0;
        let matches = part.sources.iter().all(|source| {
            current.get(&source.message).is_some_and(|current| {
                let same =
                    source.render_key == Some(current.render_key) && previous < current.ordinal;
                previous = current.ordinal;
                same
            })
        });
        if !part.sources.is_empty() && matches {
            retained.insert(part.id);
            covered.extend(part.sources.iter().map(|source| source.message));
        } else {
            invalid.extend(part.sources.iter().map(|source| source.message));
        }
    }
    covered.retain(|id| !invalid.contains(id));
    RetainedCoverage {
        parts: retained,
        messages: covered,
        issued_after,
    }
}

async fn pack(
    env: &Publisher<'_>,
    (id, revision): (Uuid, i64),
    blocks: &[Block],
    meter: Meter,
    limits: Limits,
    exact: bool,
) -> Result<Option<TranscriptPlan>, WorkerError> {
    if !exact || blocks.is_empty() {
        return plan::pack(blocks, meter, limits, None)
            .map(Some)
            .map_err(settings_error);
    }
    let (local, permit) = env.model_slot(id, revision).await?;
    let mut counts = BTreeMap::new();
    loop {
        let mut missing = BTreeMap::new();
        let packed = plan::pack(
            blocks,
            meter,
            limits,
            Some(&mut |text| {
                if meter.estimate(text) > limits.context_tokens() {
                    return ExactCount::OverContext;
                }
                let key = token_key(text);
                if let Some(&count) = counts.get(&key) {
                    ExactCount::Known(count)
                } else {
                    missing.entry(key).or_insert_with(|| text.to_owned());
                    ExactCount::Pending
                }
            }),
        )
        .map_err(settings_error)?;
        if missing.is_empty() {
            if packed.chunks.iter().any(|chunk| {
                counts
                    .get(&token_key(&chunk.text))
                    .is_some_and(|&count| count > limits.context_tokens())
            }) {
                return Err(settings_error(
                    "tokenized summary chunk exceeds the model context",
                ));
            }
            drop(permit);
            drop(local);
            return Ok(Some(packed));
        }
        for (key, text) in missing {
            let Some(count) = tokenize(env, id, revision, &text).await? else {
                if !env.tokenizer_failed.load(Ordering::Relaxed) {
                    tracing::warn!(conversation = %id, revision, "summary tokenizer became unavailable; using character estimates");
                }
                return Ok(None);
            };
            counts.insert(key, count);
        }
    }
}
fn token_key(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}
