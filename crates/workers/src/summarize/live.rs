mod run;

use jiff::Timestamp;
use kernel::memory::summary_backend::SummaryArtifact;
use run::{Run, populate, transcript_warnings};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

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
        manifest: SummaryArtifact,
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
    let warnings = transcript_warnings(&input);
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
    let (run_id, frozen) = resume_or_freeze(
        env,
        request,
        revision,
        &input,
        (&blocks, sources),
        (limits, profile),
    )
    .await?;
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
    commit_tree(env, id, revision, tree, indexes).await
}

async fn resume_or_freeze(
    env: &Publisher<'_>,
    request: &LiveRequest,
    revision: i64,
    input: &SummaryInput,
    (blocks, sources): (&[Block], Vec<Source>),
    (limits, profile): (Limits, [u8; 32]),
) -> Result<(Uuid, Frozen), WorkerError> {
    let id = request.conversation_id;
    let previous = env.summary.store.live_run(id).await?;
    if env.conversion.is_some() {
        let workspace =
            if let Some(previous) = previous.as_ref().filter(|run| run.revision == revision) {
                previous.id
            } else {
                env.freeze_run(id, revision, input.expected_root, encode(&())?)
                    .await?
            };
        env.token_workspace.get_or_init(|| workspace);
    }
    let resume = previous
        .as_ref()
        .filter(|run| run.revision == revision && !run.manifest.is_null())
        .map(|run| decode::<Frozen>(&run.manifest))
        .transpose()?
        .filter(|plan| {
            plan.profile == profile && plan.root == input.expected_root && plan.sources == sources
        });
    if let (Some(run), Some(frozen)) = (&previous, resume) {
        Ok((run.id, frozen))
    } else {
        let frozen = freeze(
            env, request, revision, input, blocks, sources, limits, profile,
        )
        .await?;
        ensure_eligible(env, id, revision).await?;
        let run_id = env
            .freeze_run(id, revision, input.expected_root, encode(&frozen)?)
            .await?;
        Ok((run_id, frozen))
    }
}

async fn commit_tree(
    env: &Publisher<'_>,
    id: Uuid,
    revision: i64,
    tree: PreparedSummaryTree,
    indexes: SummaryIndexes,
) -> Result<(), WorkerError> {
    if let Some((conversion, lease)) = env.conversion {
        return if conversion.publish(lease, tree, indexes).await? {
            Ok(())
        } else {
            Err(WorkerError::ConversionYield)
        };
    }
    let store = env.summary.store;
    match store.publish_live(tree, indexes).await {
        Ok(false) if store.summary_eligible(id, revision).await? => Err(WorkerError::SummaryReplan),
        Ok(_) => Ok(()),
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
        (request.conversation_id, revision),
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
        let coverage = retained_coverage(&sources, &input.parts);
        retained = coverage.parts;
        issued_after = coverage.issued_after;
        if !retained.is_empty() {
            chunks = Vec::new();
            for uncovered in blocks.split(|block| coverage.messages.contains(&block.source.id)) {
                chunks.extend(
                    pack(
                        env,
                        (request.conversation_id, revision),
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

struct RetainedCoverage {
    parts: BTreeSet<Uuid>,
    messages: BTreeSet<Uuid>,
    issued_after: i32,
}

fn retained_coverage(sources: &[Source], parts: &[PreparedSummaryRecord]) -> RetainedCoverage {
    let mut retained = BTreeSet::new();
    let mut issued_after = 0;
    let current: BTreeMap<_, _> = sources.iter().map(|source| (source.id, source)).collect();
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
            return decode(&count);
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
    (id, revision): (Uuid, i64),
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
                if let Some(&count) = counts.get(&key) {
                    Ok(count)
                } else {
                    missing.entry(key).or_insert_with(|| text.to_owned());
                    Ok(0)
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
                .ok_or(PlanError::ExactCount)
                .map_err(settings_error)?;
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

pub(super) fn integer(value: usize) -> Result<i32, WorkerError> {
    i32::try_from(value).map_err(|_| WorkerError::Count)
}

fn digest(value: &impl Serialize) -> Result<[u8; 32], WorkerError> {
    let bytes = serde_json::to_vec(value).map_err(WorkerError::summary_payload)?;
    Ok(Sha256::digest(bytes).into())
}

fn encode(value: &impl Serialize) -> Result<SummaryArtifact, WorkerError> {
    SummaryArtifact::encode(value).map_err(WorkerError::summary_payload)
}

fn decode<T: serde::de::DeserializeOwned>(value: &SummaryArtifact) -> Result<T, WorkerError> {
    value.decode().map_err(WorkerError::summary_payload)
}

fn settings_error(error: impl std::fmt::Display) -> WorkerError {
    WorkerError::SummarySettings {
        message: error.to_string(),
    }
}
