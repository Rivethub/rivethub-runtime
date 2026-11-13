use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use workers::entity_builder::EntityExample;
use workers::entity_builder::EntityRunReport;
use workers::entity_builder::FactCarryReport;
use workers::entity_builder::FactExample;
use workers::entity_builder::MergeExample;
use workers::entity_builder::RejectedExample;

use crate::drive::JobOutcome;
use crate::failure::Failure;
use crate::failure::redact;
use crate::logged::Totals;

pub(crate) struct StepInput<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) index: usize,
    pub(crate) conversation: Uuid,
    pub(crate) processed: &'a [Uuid],
    pub(crate) jobs: &'a [JobOutcome],
    pub(crate) llm: Totals,
    pub(crate) wall_ms: u64,
    pub(crate) pages_from: &'a Path,
    pub(crate) pool: &'a PgPool,
    pub(crate) secrets: &'a [String],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StepRecord {
    pub(crate) step: u32,
    pub(crate) conversation_id: Uuid,
    pub(crate) summaries: u32,
    pub(crate) ok: u32,
    pub(crate) failed: u32,
    pub(crate) skipped: u32,
    pub(crate) not_run: u32,
    pub(crate) work: u32,
    pub(crate) errors: u32,
    pub(crate) llm_calls: u64,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) wall_ms: u64,
    pub(crate) pages: u64,
    pub(crate) suffixed_slugs: i64,
    pub(crate) collisions: i64,
    pub(crate) extractions_not_done: i64,
}

pub(crate) struct RunInput<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) label: &'a str,
    pub(crate) slice: &'a Path,
    pub(crate) model: &'a str,
    pub(crate) base_url: &'a str,
    pub(crate) concurrency: u32,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) wall_ms: u64,
    pub(crate) setup_ms: u64,
    pub(crate) steps: &'a [StepRecord],
    pub(crate) log_error: Option<String>,
    pub(crate) harness_error: Option<String>,
    pub(crate) carry: FactCarryReport,
    pub(crate) entities: EntityRunReport,
}

#[derive(Debug)]
pub(crate) struct Printed {
    pub(crate) work: u32,
    pub(crate) errors: u32,
    pub(crate) suffixed_slugs: i64,
    pub(crate) collisions: i64,
    pub(crate) extractions_not_done: i64,
    pub(crate) llm_calls: u64,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) rewrites_with_carry_forward: u64,
    pub(crate) facts_carried: u64,
    pub(crate) facts_retired: u64,
    pub(crate) retirements_rejected: u64,
    pub(crate) carried_examples: Vec<FactExample>,
    pub(crate) retired_examples: Vec<FactExample>,
    pub(crate) rejected_examples: Vec<RejectedExample>,
    pub(crate) no_evidence_skips: u64,
    pub(crate) empty_rewrites: u64,
    pub(crate) entity_errors: u64,
    pub(crate) no_evidence_examples: Vec<EntityExample>,
    pub(crate) empty_rewrite_examples: Vec<EntityExample>,
    pub(crate) entity_error_examples: Vec<EntityExample>,
    pub(crate) passage_chars: u64,
    pub(crate) excerpted_messages: u64,
    pub(crate) excerpt_ratio: String,
    pub(crate) verified_refs: u64,
    pub(crate) redirected_refs: u64,
    pub(crate) aliases_accepted: u64,
    pub(crate) aliases_rejected: u64,
    pub(crate) aliases_paired: u64,
    pub(crate) merges: u64,
    pub(crate) redirected_examples: Vec<EntityExample>,
    pub(crate) alias_rejected_examples: Vec<EntityExample>,
    pub(crate) merge_examples: Vec<MergeExample>,
}

pub(crate) async fn snapshot(input: &StepInput<'_>) -> Result<StepRecord, Failure> {
    let step = u32::try_from(input.index.saturating_add(1)).unwrap_or(u32::MAX);
    let dir = input.dir.join(format!("step-{step}"));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|source| Failure::Write {
            path: dir.clone(),
            source,
        })?;
    let pages = copy_tree(input.pages_from, &dir.join("pages")).await?;
    let dump = dump_db(input.pool, input.secrets).await?;
    write_file(&dir.join("db.json"), dump.as_bytes()).await?;
    let totals = tally(input.jobs);
    let invariants = load_invariants(input.pool, input.processed, input.secrets).await?;
    let record = StepRecord {
        step,
        conversation_id: input.conversation,
        summaries: totals.summaries,
        ok: totals.ok,
        failed: totals.failed,
        skipped: totals.skipped,
        not_run: totals.not_run,
        work: totals.work,
        errors: totals.errors,
        llm_calls: input.llm.calls,
        prompt_tokens: input.llm.prompt_tokens,
        completion_tokens: input.llm.completion_tokens,
        wall_ms: input.wall_ms,
        pages,
        suffixed_slugs: invariants.suffixed_slugs,
        collisions: invariants.collisions,
        extractions_not_done: invariants.extractions_not_done,
    };
    let body = step_json(input.jobs, &record)?;
    write_file(&dir.join("step.json"), &body).await?;
    Ok(record)
}

pub(crate) async fn write_run(input: &RunInput<'_>) -> Result<Printed, Failure> {
    let mut printed = printed(input.steps);
    printed.rewrites_with_carry_forward = input.carry.rewrites_with_carry_forward;
    printed.facts_carried = input.carry.facts_carried;
    printed.facts_retired = input.carry.facts_retired;
    printed.retirements_rejected = input.carry.retirements_rejected;
    printed
        .carried_examples
        .clone_from(&input.carry.carried_examples);
    printed
        .retired_examples
        .clone_from(&input.carry.retired_examples);
    printed
        .rejected_examples
        .clone_from(&input.carry.rejected_examples);
    printed.no_evidence_skips = input.entities.no_evidence_skips;
    printed.empty_rewrites = input.entities.empty_rewrites;
    printed.entity_errors = input.entities.entity_errors;
    printed
        .no_evidence_examples
        .clone_from(&input.entities.no_evidence_examples);
    printed
        .empty_rewrite_examples
        .clone_from(&input.entities.empty_rewrite_examples);
    printed
        .entity_error_examples
        .clone_from(&input.entities.entity_error_examples);
    printed.passage_chars = input.entities.passage_chars;
    printed.excerpted_messages = input.entities.excerpted_messages;
    printed.excerpt_ratio = input.entities.excerpt_ratio();
    printed.verified_refs = input.entities.verified_refs;
    printed.redirected_refs = input.entities.redirected_refs;
    printed.aliases_accepted = input.entities.aliases_accepted;
    printed.aliases_rejected = input.entities.aliases_rejected;
    printed.aliases_paired = input.entities.aliases_paired;
    printed.merges = input.entities.merges;
    printed
        .redirected_examples
        .clone_from(&input.entities.redirected_examples);
    printed
        .alias_rejected_examples
        .clone_from(&input.entities.alias_rejected_examples);
    printed
        .merge_examples
        .clone_from(&input.entities.merge_examples);
    let body = run_json(input, &printed)?;
    write_file(&input.dir.join("run.json"), &body).await?;
    Ok(printed)
}

fn printed(steps: &[StepRecord]) -> Printed {
    let mut printed = Printed {
        work: 0,
        errors: 0,
        suffixed_slugs: 0,
        collisions: 0,
        extractions_not_done: 0,
        llm_calls: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        rewrites_with_carry_forward: 0,
        facts_carried: 0,
        facts_retired: 0,
        retirements_rejected: 0,
        carried_examples: Vec::new(),
        retired_examples: Vec::new(),
        rejected_examples: Vec::new(),
        no_evidence_skips: 0,
        empty_rewrites: 0,
        entity_errors: 0,
        no_evidence_examples: Vec::new(),
        empty_rewrite_examples: Vec::new(),
        entity_error_examples: Vec::new(),
        passage_chars: 0,
        excerpted_messages: 0,
        excerpt_ratio: "0".to_owned(),
        verified_refs: 0,
        redirected_refs: 0,
        aliases_accepted: 0,
        aliases_rejected: 0,
        aliases_paired: 0,
        merges: 0,
        redirected_examples: Vec::new(),
        alias_rejected_examples: Vec::new(),
        merge_examples: Vec::new(),
    };
    for step in steps {
        printed.work = printed.work.saturating_add(step.work);
        printed.errors = printed.errors.saturating_add(step.errors);
        printed.llm_calls = printed.llm_calls.saturating_add(step.llm_calls);
        printed.prompt_tokens = printed.prompt_tokens.saturating_add(step.prompt_tokens);
        printed.completion_tokens = printed
            .completion_tokens
            .saturating_add(step.completion_tokens);
        printed.suffixed_slugs = step.suffixed_slugs;
        printed.collisions = step.collisions;
        printed.extractions_not_done = step.extractions_not_done;
    }
    printed
}

#[derive(Debug, Clone, Copy)]
struct Tally {
    summaries: u32,
    ok: u32,
    failed: u32,
    skipped: u32,
    not_run: u32,
    work: u32,
    errors: u32,
}

fn tally(jobs: &[JobOutcome]) -> Tally {
    let mut tally = Tally {
        summaries: count(jobs.len()),
        ok: 0,
        failed: 0,
        skipped: 0,
        not_run: 0,
        work: 0,
        errors: 0,
    };
    for job in jobs {
        match job.outcome {
            "ok" => tally.ok = tally.ok.saturating_add(1),
            "failed" => tally.failed = tally.failed.saturating_add(1),
            "skipped" => tally.skipped = tally.skipped.saturating_add(1),
            _ => tally.not_run = tally.not_run.saturating_add(1),
        }
    }
    tally.work = tally.ok.saturating_add(tally.skipped);
    tally.errors = tally.failed.saturating_add(tally.not_run);
    tally
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

fn step_json(jobs: &[JobOutcome], record: &StepRecord) -> Result<Vec<u8>, Failure> {
    let body = StepJson {
        step: record.step,
        conversation_id: record.conversation_id,
        summaries: record.summaries,
        ok: record.ok,
        failed: record.failed,
        skipped: record.skipped,
        not_run: record.not_run,
        work: record.work,
        errors: record.errors,
        jobs: jobs.iter().map(JobJson::from).collect(),
        llm_calls: record.llm_calls,
        prompt_tokens: record.prompt_tokens,
        completion_tokens: record.completion_tokens,
        wall_ms: record.wall_ms,
        pages: record.pages,
        invariants: invariants_json(record),
    };
    serde_json::to_vec_pretty(&body).map_err(|error| Failure::Report(error.to_string()))
}

fn run_json(input: &RunInput<'_>, printed: &Printed) -> Result<Vec<u8>, Failure> {
    let body = RunJson {
        label: input.label.to_owned(),
        slice: input.slice.display().to_string(),
        model: input.model.to_owned(),
        base_url: input.base_url.to_owned(),
        concurrency: input.concurrency,
        started_at: input.started_at,
        finished_at: Utc::now(),
        wall_ms: input.wall_ms,
        setup_ms: input.setup_ms,
        summaries: input
            .steps
            .iter()
            .fold(0, |sum, step| sum.saturating_add(step.summaries)),
        ok: input
            .steps
            .iter()
            .fold(0, |sum, step| sum.saturating_add(step.ok)),
        failed: input
            .steps
            .iter()
            .fold(0, |sum, step| sum.saturating_add(step.failed)),
        skipped: input
            .steps
            .iter()
            .fold(0, |sum, step| sum.saturating_add(step.skipped)),
        not_run: input
            .steps
            .iter()
            .fold(0, |sum, step| sum.saturating_add(step.not_run)),
        work: printed.work,
        errors: printed.errors,
        llm_calls: printed.llm_calls,
        prompt_tokens: printed.prompt_tokens,
        completion_tokens: printed.completion_tokens,
        steps: input.steps.iter().map(step_summary).collect(),
        invariants: Invariants {
            suffixed_slugs: printed.suffixed_slugs,
            collisions: printed.collisions,
            extractions_not_done: printed.extractions_not_done,
        },
        fact_carry: FactCarryJson::from(&input.carry),
        entity_outcomes: EntityOutcomeJson::from(&input.entities),
        notes: NOTES.to_vec(),
        log_error: input.log_error.clone(),
        harness_error: input.harness_error.clone(),
    };
    serde_json::to_vec_pretty(&body).map_err(|error| Failure::Report(error.to_string()))
}

fn step_summary(record: &StepRecord) -> StepSummary {
    StepSummary {
        step: record.step,
        conversation_id: record.conversation_id,
        summaries: record.summaries,
        ok: record.ok,
        failed: record.failed,
        skipped: record.skipped,
        not_run: record.not_run,
        work: record.work,
        errors: record.errors,
        llm_calls: record.llm_calls,
        prompt_tokens: record.prompt_tokens,
        completion_tokens: record.completion_tokens,
        wall_ms: record.wall_ms,
        pages: record.pages,
        invariants: invariants_json(record),
    }
}

fn invariants_json(record: &StepRecord) -> Invariants {
    Invariants {
        suffixed_slugs: record.suffixed_slugs,
        collisions: record.collisions,
        extractions_not_done: record.extractions_not_done,
    }
}

#[derive(Serialize)]
struct StepJson {
    step: u32,
    conversation_id: Uuid,
    summaries: u32,
    ok: u32,
    failed: u32,
    skipped: u32,
    not_run: u32,
    work: u32,
    errors: u32,
    jobs: Vec<JobJson>,
    llm_calls: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    wall_ms: u64,
    pages: u64,
    invariants: Invariants,
}

#[derive(Serialize)]
struct RunJson {
    label: String,
    slice: String,
    model: String,
    base_url: String,
    concurrency: u32,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    wall_ms: u64,
    setup_ms: u64,
    summaries: u32,
    ok: u32,
    failed: u32,
    skipped: u32,
    not_run: u32,
    work: u32,
    errors: u32,
    llm_calls: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    steps: Vec<StepSummary>,
    invariants: Invariants,
    fact_carry: FactCarryJson,
    entity_outcomes: EntityOutcomeJson,
    notes: Vec<&'static str>,
    log_error: Option<String>,
    harness_error: Option<String>,
}

#[derive(Serialize)]
struct StepSummary {
    step: u32,
    conversation_id: Uuid,
    summaries: u32,
    ok: u32,
    failed: u32,
    skipped: u32,
    not_run: u32,
    work: u32,
    errors: u32,
    llm_calls: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    wall_ms: u64,
    pages: u64,
    invariants: Invariants,
}

#[derive(Serialize)]
struct JobJson {
    summary_id: Uuid,
    outcome: &'static str,
    error: Option<String>,
}

impl From<&JobOutcome> for JobJson {
    fn from(job: &JobOutcome) -> Self {
        Self {
            summary_id: job.summary_id,
            outcome: job.outcome,
            error: job.error.clone(),
        }
    }
}

#[derive(Serialize)]
struct FactCarryJson {
    rewrites_with_carry_forward: u64,
    facts_carried: u64,
    facts_retired: u64,
    retirements_rejected: u64,
    carried_examples: Vec<FactExample>,
    retired_examples: Vec<FactExample>,
    rejected_examples: Vec<RejectedExample>,
}

impl From<&FactCarryReport> for FactCarryJson {
    fn from(report: &FactCarryReport) -> Self {
        Self {
            rewrites_with_carry_forward: report.rewrites_with_carry_forward,
            facts_carried: report.facts_carried,
            facts_retired: report.facts_retired,
            retirements_rejected: report.retirements_rejected,
            carried_examples: report.carried_examples.clone(),
            retired_examples: report.retired_examples.clone(),
            rejected_examples: report.rejected_examples.clone(),
        }
    }
}

#[derive(Serialize)]
struct EntityOutcomeJson {
    no_evidence_skips: u64,
    empty_rewrites: u64,
    entity_errors: u64,
    no_evidence_examples: Vec<EntityExample>,
    empty_rewrite_examples: Vec<EntityExample>,
    entity_error_examples: Vec<EntityExample>,
    passage_chars: u64,
    excerpted_messages: u64,
    excerpt_chars: u64,
    excerpt_source_chars: u64,
    excerpt_ratio: String,
    verified_refs: u64,
    redirected_refs: u64,
    aliases_accepted: u64,
    aliases_rejected: u64,
    aliases_paired: u64,
    merges: u64,
    redirected_examples: Vec<EntityExample>,
    alias_rejected_examples: Vec<EntityExample>,
    merge_examples: Vec<MergeExample>,
}

impl From<&EntityRunReport> for EntityOutcomeJson {
    fn from(report: &EntityRunReport) -> Self {
        Self {
            no_evidence_skips: report.no_evidence_skips,
            empty_rewrites: report.empty_rewrites,
            entity_errors: report.entity_errors,
            no_evidence_examples: report.no_evidence_examples.clone(),
            empty_rewrite_examples: report.empty_rewrite_examples.clone(),
            entity_error_examples: report.entity_error_examples.clone(),
            passage_chars: report.passage_chars,
            excerpted_messages: report.excerpted_messages,
            excerpt_chars: report.excerpt_chars,
            excerpt_source_chars: report.excerpt_source_chars,
            excerpt_ratio: report.excerpt_ratio(),
            verified_refs: report.verified_refs,
            redirected_refs: report.redirected_refs,
            aliases_accepted: report.aliases_accepted,
            aliases_rejected: report.aliases_rejected,
            aliases_paired: report.aliases_paired,
            merges: report.merges,
            redirected_examples: report.redirected_examples.clone(),
            alias_rejected_examples: report.alias_rejected_examples.clone(),
            merge_examples: report.merge_examples.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, sqlx::FromRow)]
struct Invariants {
    suffixed_slugs: i64,
    collisions: i64,
    extractions_not_done: i64,
}

async fn load_invariants(
    pool: &PgPool,
    ids: &[Uuid],
    secrets: &[String],
) -> Result<Invariants, Failure> {
    sqlx::query_as::<_, Invariants>(INVARIANTS)
        .bind(ids)
        .fetch_one(pool)
        .await
        .map_err(|error| Failure::Report(redact(&error.to_string(), secrets)))
}

async fn dump_db(pool: &PgPool, secrets: &[String]) -> Result<String, Failure> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    let mut root = serde_json::Map::new();
    for (table, order) in DUMP_TABLES {
        let value = fetch_table(pool, table, order, secrets).await?;
        root.insert((*table).to_owned(), value);
    }
    serde_json::to_string_pretty(&Value::Object(root))
        .map_err(|error| Failure::Report(error.to_string()))
}

async fn fetch_table(
    pool: &PgPool,
    table: &str,
    order: &str,
    secrets: &[String],
) -> Result<Value, Failure> {
    let columns = sqlx::query_scalar::<_, String>(COLUMNS)
        .bind(format!("public.{table}"))
        .fetch_all(pool)
        .await
        .map_err(|error| Failure::Report(redact(&error.to_string(), secrets)))?;
    let list = column_list(&columns)?;
    let sql = format!(
        "SELECT COALESCE((SELECT jsonb_agg(to_jsonb(x) ORDER BY {order}) \
         FROM (SELECT {list} FROM {table}) x), '[]'::jsonb)::text"
    );
    let text: String = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .map_err(|error| Failure::Report(redact(&error.to_string(), secrets)))?;
    serde_json::from_str(&text).map_err(|error| Failure::Report(error.to_string()))
}

fn column_list(columns: &[String]) -> Result<String, Failure> {
    if columns.is_empty() {
        return Err(Failure::Report("a dump table has no columns".to_owned()));
    }
    let mut parts = Vec::with_capacity(columns.len());
    for column in columns {
        if !ident_ok(column) {
            return Err(Failure::Report(format!(
                "column {column} is not a plain identifier"
            )));
        }
        parts.push(format!("\"{column}\""));
    }
    Ok(parts.join(", "))
}

fn ident_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

async fn copy_tree(from: &Path, to: &Path) -> Result<u64, Failure> {
    tokio::fs::create_dir_all(to)
        .await
        .map_err(|source| Failure::Write {
            path: to.to_path_buf(),
            source,
        })?;
    if !tokio::fs::try_exists(from)
        .await
        .map_err(|source| Failure::Write {
            path: from.to_path_buf(),
            source,
        })?
    {
        return Ok(0);
    }
    let mut pages = 0u64;
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((dir, dest)) = pending.pop() {
        pages = pages.saturating_add(copy_dir(&dir, &dest, &mut pending).await?);
    }
    Ok(pages)
}

async fn copy_dir(
    dir: &Path,
    dest: &Path,
    pending: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<u64, Failure> {
    let mut read = tokio::fs::read_dir(dir)
        .await
        .map_err(|source| Failure::Write {
            path: dir.to_path_buf(),
            source,
        })?;
    let mut pages = 0u64;
    while let Some(entry) = read.next_entry().await.map_err(|source| Failure::Write {
        path: dir.to_path_buf(),
        source,
    })? {
        pages = pages.saturating_add(copy_entry(&entry, dest, pending).await?);
    }
    Ok(pages)
}

async fn copy_entry(
    entry: &tokio::fs::DirEntry,
    dest: &Path,
    pending: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<u64, Failure> {
    let kind = entry.file_type().await.map_err(|source| Failure::Write {
        path: entry.path(),
        source,
    })?;
    let target = dest.join(entry.file_name());
    if kind.is_dir() {
        tokio::fs::create_dir_all(&target)
            .await
            .map_err(|source| Failure::Write {
                path: target.clone(),
                source,
            })?;
        pending.push((entry.path(), target));
        return Ok(0);
    }
    tokio::fs::copy(entry.path(), &target)
        .await
        .map_err(|source| Failure::Write {
            path: target.clone(),
            source,
        })?;
    Ok(u64::from(is_markdown(&target)))
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("md")
}

async fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    tokio::fs::write(path, bytes)
        .await
        .map_err(|source| Failure::Write {
            path: path.to_path_buf(),
            source,
        })
}

const NOTES: &[&str] = &[
    "lineage is empty: sessions placed only through on-disk parent transcripts stay unplaced",
    "the scratch role is the container superuser, so row level security is bypassed; the session still sets rh.maintenance=on and an empty principal",
    "live rows are copied with COPY under rh.principal and default_transaction_read_only; embedding columns are left null",
    "conversations run oldest-first on one scratch database; each lead enqueues itself and the next 7 later same-topic leaves of that conversation",
    "the provider drops reasoning-channel text before chat returns, so llm logs keep the prompt, content, and reasoning token counts",
    "embed jobs are enqueued and not run",
    "RIVETHUB_WIKI_EXTRACTION gates the worker process; this harness calls handle_extract_wiki directly",
    "a current fact the model omits stays current unless this session renames it or contradicts it with a quote from the passages; fact_carry counts those rewrites, the facts carried, the facts retired, and the retirements rejected",
    "an entity with no passage is skipped, an empty rewrite is dropped or kept as a mention, and any other per-entity failure stays with that entity; entity_outcomes counts those and keeps up to five examples",
    "a kept message longer than 4000 characters is excerpted to its header and a window around each mention of the entity; passage_chars is the passage text sent, excerpted_messages counts those messages, and excerpt_ratio is excerpt chars divided by their original chars",
    "an identify entity_ref is kept only when the observation name already matches that entity; otherwise the matcher runs and the ref is counted in redirected_refs. A new alias is counted in aliases_accepted when the alias check reaches 0.6, aliases_paired counts a mention that already pairs the name and the alias, and aliases_rejected keeps up to ten examples with the probability",
];

const DUMP_TABLES: &[(&str, &str)] = &[
    ("rh_topics", "x.id"),
    ("rh_topic_aliases", "x.kind, x.alias_key"),
    ("rh_wiki_topics", "x.slug"),
    ("rh_wiki_entity_candidates", "x.key, x.source"),
    ("rh_wiki_extractions", "x.summary_id"),
    ("rh_wiki_redirects", "x.from_slug"),
];

const COLUMNS: &str = "\
SELECT a.attname::text
  FROM pg_attribute a
  JOIN pg_type t ON t.oid = a.atttypid
 WHERE a.attrelid = to_regclass($1)
   AND a.attnum > 0
   AND NOT a.attisdropped
   AND a.attgenerated = ''
   AND t.typname NOT IN ('vector', 'halfvec')
 ORDER BY a.attnum";

const INVARIANTS: &str = "\
SELECT
    (
        (SELECT count(*) FROM rh_topics WHERE slug ~ '-[0-9]+$')
        + (SELECT count(*) FROM rh_wiki_topics WHERE slug ~ '-[0-9]+$')
        + (SELECT count(*) FROM rh_wiki_topics WHERE local_slug ~ '-[0-9]+$')
    )::bigint AS suffixed_slugs,
    (
        SELECT count(*) FROM (
            SELECT name FROM (
                SELECT lower(btrim(title)) AS name, slug FROM rh_wiki_topics
                UNION ALL
                SELECT lower(btrim(alias)), slug
                  FROM rh_wiki_topics, unnest(aliases) AS alias
            ) names
            WHERE name <> ''
            GROUP BY name
            HAVING count(DISTINCT slug) > 1
        ) collisions
    )::bigint AS collisions,
    (
        SELECT count(*) FROM rh_summaries s
        WHERE s.kind = 'leaf'
          AND s.conversation_id = ANY($1)
          AND NOT EXISTS (
              SELECT 1 FROM rh_wiki_extractions e
              WHERE e.summary_id = s.id AND e.status = 'done'
          )
    )::bigint AS extractions_not_done";
