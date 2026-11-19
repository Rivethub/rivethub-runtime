use std::collections::VecDeque;

use super::*;

pub(super) async fn freeze(
    env: &Publisher<'_>,
    id: (Uuid, i64),
    input: &SummaryInput,
    (blocks, sources): (&[Block], Vec<Source>),
    (limits, profile): (Limits, [u8; 32]),
) -> Result<Frozen, WorkerError> {
    let calibrated = calibrate(env, id, blocks, limits).await?;
    let initial = pack_transcript(env, id, blocks, calibrated, limits, &input.parts).await?;
    let packed = if let Some(initial) = initial {
        initial
    } else {
        pack_transcript(env, id, blocks, None, limits, &input.parts)
            .await?
            .ok_or(WorkerError::Count)?
    };
    Ok(Frozen {
        profile,
        root: input.expected_root,
        sources,
        ratio: packed.meter.ratio(),
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
    meter: Meter,
    total_tokens: usize,
    coverage: RetainedCoverage,
    chunks: Vec<Chunk>,
    direct: bool,
}

async fn pack_transcript(
    env: &Publisher<'_>,
    id: (Uuid, i64),
    blocks: &[Block],
    calibrated: Option<Meter>,
    limits: Limits,
    parts: &[PreparedSummaryRecord],
) -> Result<Option<PackedTranscript>, WorkerError> {
    let meter = calibrated.unwrap_or_else(Meter::uncalibrated);
    let exact = calibrated.is_some();
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
        meter,
        total_tokens: all.total_tokens,
        coverage,
        chunks,
        direct,
    }))
}

#[derive(Default)]
pub(super) struct RetainedCoverage {
    pub(super) parts: BTreeSet<Uuid>,
    pub(super) messages: BTreeSet<Uuid>,
    issued_after: i32,
}

pub(super) fn retained_coverage<'a>(
    sources: impl Iterator<Item = &'a Source>,
    parts: &[PreparedSummaryRecord],
) -> RetainedCoverage {
    let current: BTreeMap<_, _> = sources.map(|source| (source.id, source)).collect();
    let issued_after = parts
        .iter()
        .filter_map(|part| part.issued_part_ordinal)
        .fold(0, i32::max);
    let mut owners = BTreeMap::<_, Vec<_>>::new();
    let mut rejected = VecDeque::new();
    for part in parts {
        for source in &part.sources {
            owners.entry(source.message).or_default().push(part);
        }
        if !part_matches(part, &current) {
            rejected.push_back(part);
        }
    }
    let mut invalid = BTreeSet::new();
    while let Some(part) = rejected.pop_front() {
        if invalid.insert(part.id) {
            for source in &part.sources {
                if let Some(siblings) = owners.remove(&source.message) {
                    rejected.extend(siblings);
                }
            }
        }
    }
    RetainedCoverage {
        parts: parts
            .iter()
            .map(|part| part.id)
            .filter(|id| !invalid.contains(id))
            .collect(),
        messages: owners.into_keys().collect(),
        issued_after,
    }
}

fn part_matches(part: &PreparedSummaryRecord, current: &BTreeMap<Uuid, &Source>) -> bool {
    let mut previous = 0;
    !part.sources.is_empty()
        && part.sources.iter().all(|source| {
            current.get(&source.message).is_some_and(|current| {
                let same =
                    source.render_key == Some(current.render_key) && previous < current.ordinal;
                previous = current.ordinal;
                same
            })
        })
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
        if packed.chunks.iter().any(|chunk| {
            counts
                .get(&token_key(&chunk.text))
                .is_some_and(|&count| count > limits.context_tokens())
        }) {
            return Err(settings_error(
                "tokenized summary chunk exceeds the model context",
            ));
        }
        if missing.is_empty() {
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
