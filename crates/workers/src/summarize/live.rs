use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, stream};
use kernel::memory::{
    SummaryKind,
    summary_backend::{SummaryIndex, SummaryIndexes, SummaryInput, SummaryPermit},
    summary_conversion::{ConversionCharge, ConversionLease, ConversionUsage, SummaryConversion},
    summary_reference::{
        PreparedSummaryRecord, PreparedSummaryTree, SummaryManifestEntry, SummarySectionBounds,
    },
};
use kernel::model::ModelProvider;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::top_down::{
    llm::{self, Reply},
    plan::{self, Block, Chunk, ComposePlan, Meter, PlanError, Source, TranscriptPlan},
    prompt::{self, Mode, Prompt},
    settings::{self, Limits},
};
use super::{SummarizeEnv, index::prepare_index};
use crate::{EmbedSettings, WorkerError};

pub(crate) const ATTEMPTS: i32 = 3;
const INPUT_WARNING: u32 = 1 << 4;
const CHAT_FRAMING_TOKENS: usize = 32;
const CONVERSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRequest {
    pub conversation_id: Uuid,
    pub revision: Option<i64>,
}

#[expect(missing_debug_implementations)]
pub struct LiveEnv<'a> {
    pub summary: &'a SummarizeEnv<'a>,
    pub embedder: &'a dyn ModelProvider,
    pub embed: EmbedSettings,
    pub permits: &'a Semaphore,
}

struct Publisher<'a> {
    live: &'a LiveEnv<'a>,
    conversion: Option<(&'a dyn SummaryConversion, ConversionLease)>,
    token_workspace: OnceLock<Uuid>,
}

impl<'a> std::ops::Deref for Publisher<'a> {
    type Target = LiveEnv<'a>;

    fn deref(&self) -> &Self::Target {
        self.live
    }
}

impl Publisher<'_> {
    async fn boundary(&self, charge: Option<ConversionCharge>) -> Result<(), WorkerError> {
        if let Some((store, lease)) = self.conversion
            && !store.admit(lease, charge).await?
        {
            return Err(WorkerError::ConversionYield);
        }
        Ok(())
    }

    async fn freeze_run(
        &self,
        id: Uuid,
        revision: i64,
        root: Option<Uuid>,
        manifest: serde_json::Value,
    ) -> Result<Uuid, WorkerError> {
        if let Some((store, lease)) = self.conversion {
            return store
                .freeze(lease, root, manifest)
                .await?
                .ok_or(WorkerError::ConversionYield);
        }
        Ok(self
            .summary
            .store
            .freeze_live_run(id, revision, root, manifest)
            .await?)
    }

    async fn model_slot(
        &self,
        id: Uuid,
        revision: i64,
    ) -> Result<(tokio::sync::SemaphorePermit<'_>, Box<dyn SummaryPermit>), WorkerError> {
        if let Some((store, _)) = self.conversion {
            let Ok(mut local) = self.permits.try_acquire_many(2) else {
                return Err(WorkerError::ConversionYield);
            };
            drop(local.split(1));
            let permit = store
                .try_model_slot(self.summary.settings.model_concurrency)
                .await?
                .ok_or(WorkerError::ConversionYield)?;
            Ok((local, permit))
        } else {
            let local = self
                .permits
                .acquire()
                .await
                .map_err(|_| WorkerError::Closed)?;
            let permit = admission(self, id, revision).await?;
            Ok((local, permit))
        }
    }

    async fn request<T>(
        &self,
        future: impl Future<Output = Result<T, kernel::model::ModelProviderError>>,
    ) -> Result<T, WorkerError> {
        if self.conversion.is_some() {
            tokio::time::timeout(CONVERSION_REQUEST_TIMEOUT, future)
                .await
                .map_err(|_| settings_error("conversion model request timed out"))?
                .map_err(super::llm_error)
        } else {
            future.await.map_err(super::llm_error)
        }
    }
}

pub(super) async fn convert(
    env: &LiveEnv<'_>,
    store: &dyn SummaryConversion,
    lease: ConversionLease,
) -> Result<(), WorkerError> {
    let publisher = Publisher {
        live: env,
        conversion: Some((store, lease)),
        token_workspace: OnceLock::new(),
    };
    let request = LiveRequest {
        conversation_id: lease.conversation,
        revision: Some(lease.revision),
    };
    publish(&publisher, &request, lease.revision).await
}

#[derive(Deserialize, Serialize)]
struct Frozen {
    profile: [u8; 32],
    root: Option<Uuid>,
    sources: Vec<Source>,
    ratio: f64,
    total_tokens: usize,
    retained: BTreeSet<Uuid>,
    issued_after: i32,
    chunks: Vec<Chunk>,
    direct: bool,
}

struct Run<'a> {
    env: &'a Publisher<'a>,
    id: Uuid,
    request: &'a LiveRequest,
    revision: i64,
    limits: Limits,
    meter: Meter,
    profile: [u8; 32],
}

pub async fn handle(
    env: &LiveEnv<'_>,
    request: &LiveRequest,
    attempt: u32,
) -> Result<(), WorkerError> {
    let Some(revision) = request.revision else {
        return Ok(());
    };
    let publisher = Publisher {
        live: env,
        conversion: None,
        token_workspace: OnceLock::new(),
    };
    let mut outcome = publish(&publisher, request, revision).await;
    if matches!(outcome, Err(WorkerError::SummaryReplan)) {
        outcome = publish(&publisher, request, revision).await;
    }
    match outcome {
        Err(WorkerError::SummarySuperseded) => Ok(()),
        Err(error)
            if attempt >= ATTEMPTS.unsigned_abs()
                && !matches!(
                    error,
                    WorkerError::Cancelled | WorkerError::ProviderOutage { .. }
                ) =>
        {
            env.summary
                .store
                .fail_live(request.conversation_id, revision)
                .await?;
            Err(error)
        }
        result => result,
    }
}

async fn publish(
    env: &Publisher<'_>,
    request: &LiveRequest,
    revision: i64,
) -> Result<(), WorkerError> {
    let store = env.summary.store;
    let id = request.conversation_id;
    ensure_eligible(env, id, revision).await?;
    let Some(mut input) = store.live_input(id).await? else {
        return Ok(());
    };
    let blocks = plan::render(&input.messages).map_err(settings_error)?;
    if env.conversion.is_some() && blocks.is_empty() && !input.expected_ids.is_empty() {
        return Err(settings_error(
            "no renderable transcript for the existing summary tree",
        ));
    }
    let warnings: BTreeSet<_> = input
        .messages
        .iter()
        .filter_map(|row| {
            let metadata = row.metadata.as_ref()?.as_map();
            (metadata
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                || metadata.contains_key("full_tool_result_length"))
            .then(|| row.id.as_deref()?.parse::<Uuid>().ok())
            .flatten()
        })
        .collect();
    let sources: Vec<_> = blocks.iter().map(|block| block.source.clone()).collect();
    let limits = env.summary.settings.live_limits()?;
    let summary_settings = env.summary.settings;
    let profile = digest(&(
        "top-down-s1-v1",
        &summary_settings.model,
        &summary_settings.model_profile,
        limits.context_tokens(),
        limits.chunk_tokens(),
        limits.thinking(),
    ))?;
    let previous = store.live_run(id).await?;
    if env.conversion.is_some() {
        let workspace =
            if let Some(previous) = previous.as_ref().filter(|run| run.revision == revision) {
                previous.id
            } else {
                env.freeze_run(id, revision, input.expected_root, serde_json::Value::Null)
                    .await?
            };
        let _ = env.token_workspace.set(workspace);
    }
    let resume = previous
        .as_ref()
        .filter(|run| run.revision == revision && !run.manifest.is_null())
        .map(|run| decode::<Frozen>(run.manifest.clone()))
        .transpose()?
        .filter(|plan| {
            plan.profile == profile && plan.root == input.expected_root && plan.sources == sources
        });
    let (run_id, frozen) = if let (Some(run), Some(frozen)) = (&previous, resume) {
        (run.id, frozen)
    } else {
        let frozen = freeze(
            env, request, revision, &input, &blocks, sources, limits, profile,
        )
        .await?;
        ensure_eligible(env, id, revision).await?;
        let run_id = env
            .freeze_run(id, revision, input.expected_root, encode(&frozen)?)
            .await?;
        (run_id, frozen)
    };
    let now = env.summary.now;
    let mut tree = PreparedSummaryTree {
        conversation: id,
        owner: input.owner,
        expected_ids: input.expected_ids,
        expected_revision: Some(revision),
        historical: env.conversion.is_some(),
        created_at: now,
        records: Vec::new(),
        retained_parts: frozen.retained.clone(),
        part_order: Vec::new(),
        passage_windows: BTreeMap::new(),
    };
    let mut indexes = SummaryIndexes::new();
    if !frozen.sources.is_empty() {
        let run = Run {
            env,
            id: run_id,
            request,
            revision,
            limits,
            meter: Meter::new(frozen.ratio).map_err(settings_error)?,
            profile,
        };
        input
            .parts
            .retain(|part| frozen.retained.contains(&part.id));
        populate(
            &run,
            &mut tree,
            &mut indexes,
            frozen,
            input.parts,
            &warnings,
        )
        .await?;
    }
    if let Some((conversion, lease)) = env.conversion {
        return if conversion.publish(lease, tree, indexes).await? {
            Ok(())
        } else {
            Err(WorkerError::ConversionYield)
        };
    }
    match store.publish_live(tree, indexes).await {
        Ok(true) => Ok(()),
        Ok(false) if store.summary_eligible(id, revision).await? => Err(WorkerError::SummaryReplan),
        Ok(false) => Ok(()),
        Err(error) => match store.summary_state(id).await {
            Ok(Some(state))
                if state
                    .summarized_through
                    .is_some_and(|through| through >= revision) =>
            {
                Ok(())
            }
            _ => Err(error.into()),
        },
    }
}

#[expect(clippy::too_many_arguments)]
async fn freeze(
    env: &Publisher<'_>,
    request: &LiveRequest,
    revision: i64,
    input: &SummaryInput,
    blocks: &[Block],
    sources: Vec<Source>,
    limits: Limits,
    profile: [u8; 32],
) -> Result<Frozen, WorkerError> {
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
    let exact = if sample.is_empty() {
        None
    } else {
        let (local, permit) = env.model_slot(request.conversation_id, revision).await?;
        let count = tokenize(env, request.conversation_id, revision, &sample).await?;
        drop(permit);
        drop(local);
        count.filter(|&count| count > 0)
    };
    let meter = match exact {
        Some(count) => Meter::calibrate(&sample, count).map_err(settings_error)?,
        None => Meter::uncalibrated(),
    };
    let all = pack(
        env,
        request.conversation_id,
        revision,
        blocks,
        meter,
        limits,
        exact.is_some(),
    )
    .await?;
    let direct = all.chunks.len() <= 1;
    let mut retained = BTreeSet::new();
    let mut chunks = all.chunks;
    let mut issued_after = 0;
    if !direct && !input.parts.is_empty() {
        let current: BTreeMap<_, _> = sources.iter().map(|source| (source.id, source)).collect();
        let mut covered = BTreeSet::new();
        let mut invalid = BTreeSet::new();
        for part in &input.parts {
            issued_after = issued_after.max(part.issued_part_ordinal.unwrap_or(0));
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
        if !retained.is_empty() {
            chunks = Vec::new();
            for uncovered in blocks.split(|block| covered.contains(&block.source.id)) {
                chunks.extend(
                    pack(
                        env,
                        request.conversation_id,
                        revision,
                        uncovered,
                        meter,
                        limits,
                        exact.is_some(),
                    )
                    .await?
                    .chunks,
                );
            }
        }
    }
    Ok(Frozen {
        profile,
        root: input.expected_root,
        sources,
        ratio: meter.ratio(),
        total_tokens: all.total_tokens,
        retained,
        issued_after,
        chunks,
        direct,
    })
}

async fn populate(
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
        let count = frozen.chunks.len();
        let mut jobs = stream::iter(frozen.chunks.into_iter().enumerate().map(
            |(position, chunk)| {
                let issued = usize::try_from(frozen.issued_after).unwrap_or(0) + position + 1;
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
                    part.input_tokens = Some(
                        i64::try_from(chunk.input_token_estimate)
                            .map_err(|_| WorkerError::Count)?,
                    );
                    Ok::<_, WorkerError>(part)
                }
            },
        ))
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
    let times: BTreeMap<_, _> = frozen
        .sources
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
            return Ok((key, decode(saved)?));
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
            "summary-index-v1",
            row.index_key,
            &row.content,
            sources,
            sections,
        ))?;
        let store = self.env.summary.store;
        if let Some(saved) = store.live_checkpoint(self.id, key).await? {
            return decode(saved);
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
    ) -> Pin<Box<dyn Future<Output = Result<([u8; 32], Reply), WorkerError>> + Send + 'a>> {
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

async fn admission(
    env: &Publisher<'_>,
    id: Uuid,
    revision: i64,
) -> Result<Box<dyn kernel::memory::summary_backend::SummaryPermit>, WorkerError> {
    loop {
        ensure_eligible(env, id, revision).await?;
        if let Some(permit) = env
            .summary
            .store
            .try_summary_permit(env.summary.settings.model_concurrency)
            .await?
        {
            return Ok(permit);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn tokenize(
    env: &Publisher<'_>,
    id: Uuid,
    revision: i64,
    text: &str,
) -> Result<Option<usize>, WorkerError> {
    ensure_eligible(env, id, revision).await?;
    let store = env.summary.store;
    let settings = env.summary.settings;
    let key = if let Some(&run) = env.token_workspace.get() {
        let key = digest(&(
            "conversion-tokenizer-v1",
            &settings.model,
            &settings.model_profile,
            text,
        ))?;
        if let Some(count) = store.live_checkpoint(run, key).await? {
            return decode(count);
        }
        Some((run, key))
    } else {
        None
    };
    let count = env
        .request(env.summary.provider.count_tokens(&settings.model, text))
        .await?;
    if let Some((run, key)) = key {
        store
            .save_live_checkpoint(run, revision, key, encode(&count)?)
            .await?;
    }
    Ok(count)
}

async fn pack(
    env: &Publisher<'_>,
    id: Uuid,
    revision: i64,
    blocks: &[Block],
    meter: Meter,
    limits: Limits,
    exact: bool,
) -> Result<TranscriptPlan, WorkerError> {
    if !exact || blocks.is_empty() {
        return plan::pack(blocks, meter, limits, None).map_err(settings_error);
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
                let key: [u8; 32] = Sha256::digest(text.as_bytes()).into();
                match counts.get(&key) {
                    Some(&count) => Ok(count),
                    None => {
                        missing.entry(key).or_insert_with(|| text.to_owned());
                        Ok(0)
                    }
                }
            }),
        )
        .map_err(settings_error)?;
        if missing.is_empty() {
            drop(permit);
            drop(local);
            return Ok(packed);
        }
        for (key, text) in missing {
            let count = tokenize(env, id, revision, &text)
                .await?
                .ok_or_else(|| settings_error("summary tokenizer became unavailable"))?;
            counts.insert(key, count);
        }
    }
}

async fn ensure_eligible(env: &Publisher<'_>, id: Uuid, revision: i64) -> Result<(), WorkerError> {
    super::llm::cancelled(env.summary)?;
    if env.conversion.is_some() {
        return env.boundary(None).await;
    }
    if env.summary.store.summary_eligible(id, revision).await? {
        Ok(())
    } else {
        Err(WorkerError::SummarySuperseded)
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
    times: &BTreeMap<Uuid, DateTime<Utc>>,
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
                let _ = write!(result, " [{}-{}]", plan::tag(first), plan::tag(last));
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
            content: chars[section.chars.clone()].iter().collect(),
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

pub(super) fn integer(value: usize) -> Result<i32, WorkerError> {
    i32::try_from(value).map_err(|_| WorkerError::Count)
}

fn digest(value: &impl Serialize) -> Result<[u8; 32], WorkerError> {
    let bytes = serde_json::to_vec(value).map_err(WorkerError::summary_payload)?;
    Ok(Sha256::digest(bytes).into())
}

fn encode(value: &impl Serialize) -> Result<serde_json::Value, WorkerError> {
    serde_json::to_value(value).map_err(WorkerError::summary_payload)
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, WorkerError> {
    serde_json::from_value(value).map_err(WorkerError::summary_payload)
}

fn settings_error(error: impl std::fmt::Display) -> WorkerError {
    WorkerError::SummarySettings {
        message: error.to_string(),
    }
}
