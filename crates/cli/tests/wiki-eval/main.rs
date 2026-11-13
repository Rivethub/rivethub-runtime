mod attempt;
mod copy;
mod drive;
mod failure;
mod lineage;
mod logged;
mod report;
mod scratch;
mod slice;

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use std::time::Instant;

use chrono::Utc;
use clap::Parser;

use failure::Failure;
use failure::note;
use failure::secrets;
use logged::Totals;

#[derive(Parser)]
#[command(
    name = "wiki-eval",
    about = "Replays extract-wiki over a conversation slice"
)]
struct Args {
    #[arg(long, value_name = "FILE")]
    slice: PathBuf,
    #[arg(long)]
    label: String,
    #[arg(long)]
    source_url: String,
    #[arg(long)]
    scratch_url: String,
    #[arg(long, default_value = "phil")]
    principal: String,
    #[arg(long, default_value = "thinkingcap-flash-next")]
    model: String,
    #[arg(long, default_value = "http://127.0.0.1:8003/v1")]
    base_url: String,

    #[arg(long, value_name = "FILE")]
    api_key_file: Option<PathBuf>,

    #[arg(long)]
    no_reasoning: bool,
    #[arg(long, default_value_t = 1)]
    concurrency: u32,
    #[arg(long, default_value = "entities", value_name = "entities|topics|both")]
    worker: String,
    #[arg(long, default_value = "thinkingcap", value_name = "thinkingcap|jev")]
    matcher: String,
    #[arg(long, default_value = "")]
    matcher_url: String,
    #[arg(long, default_value = "/home/phil/work/entity-eval/runs")]
    runs: PathBuf,

    #[arg(long)]
    until: Option<String>,
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let installed = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    let _installed = installed.is_err();
}

fn main() -> ExitCode {
    init_tracing();
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return failure::report(&Failure::Runtime(error)),
    };
    match runtime.block_on(run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failure::report(&error),
    }
}

async fn run(args: &Args) -> Result<(), Failure> {
    let matcher = check(args)?;
    let ids = slice::load(&args.slice)?;
    let hidden = secrets(&[&args.source_url, &args.scratch_url]);
    preflight(&args.base_url).await?;
    let setup = Instant::now();
    let db = scratch::name(&args.label);
    let databases = scratch::databases(&args.scratch_url, &db)?;
    scratch::fresh(&databases.admin, &db, &hidden).await?;
    scratch::migrate(&databases.app, &hidden).await?;
    let counts = copy::copy_slice(
        &databases.app,
        &args.source_url,
        &args.principal,
        &ids,
        &hidden,
    )
    .await?;
    copied(&counts, ids.len())?;
    if let Some(until) = args.until.as_deref() {
        copy::trim_after(&databases.app, until, &hidden).await?;
    }
    let setup_ms = millis(setup.elapsed());
    let dir = prepare_dir(&args.runs, &args.label).await?;
    let started_at = Utc::now();
    let wall = Instant::now();
    let prepared = Prepared {
        args,
        app: &databases.app,
        dir: &dir,
        ids: &ids,
        hidden: &hidden,
        started_at,
        wall,
        setup_ms,
        matcher: &matcher,
    };
    kernel::tenancy::Scope::Maintenance
        .run(finish(prepared))
        .await
}

struct Prepared<'a> {
    args: &'a Args,
    app: &'a str,
    dir: &'a Path,
    ids: &'a [uuid::Uuid],
    hidden: &'a [String],
    started_at: chrono::DateTime<Utc>,
    wall: Instant,
    setup_ms: u64,
    matcher: &'a workers::entity_builder::MatcherSettings,
}

async fn finish(prepared: Prepared<'_>) -> Result<(), Failure> {
    let engine = match drive::open(&drive::Run {
        app: prepared.app,
        pages: &prepared.dir.join("pages"),
        llm_dir: &prepared.dir.join("llm"),
        principal: &prepared.args.principal,
        model: &prepared.args.model,
        base_url: &prepared.args.base_url,
        api_key_file: prepared.args.api_key_file.as_deref(),
        no_reasoning: prepared.args.no_reasoning,
        secrets: prepared.hidden,
        worker: drive::Workers::parse(&prepared.args.worker)?,
        matcher: prepared.matcher,
    })
    .await
    {
        Ok(engine) => engine,
        Err(error) => return close(prepared, &[], Some(error), None).await,
    };
    let order = match drive::order(&engine, prepared.ids).await {
        Ok(order) => order,
        Err(error) => {
            let log_error = engine.log_error().await;
            return close(prepared, &[], Some(error), log_error).await;
        }
    };
    let mut steps = Vec::new();
    let mut processed = Vec::new();
    let mut failure = None;
    let concurrency = usize::try_from(prepared.args.concurrency).unwrap_or(usize::MAX);
    for (index, id) in order.into_iter().enumerate() {
        processed.push(id);
        match run_step(&engine, &prepared, index, id, &processed, concurrency).await {
            Ok((record, step_failure)) => {
                let emitted = emit_step(&record);
                steps.push(record);
                if let Err(error) = emitted {
                    failure = Some(error);
                    break;
                }
                if let Some(error) = step_failure {
                    failure = Some(error);
                    break;
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let log_error = engine.log_error().await;
    if let Ok(count) = drive::held_count(&engine).await {
        emit_line(&format!("held_for_review {count}"))?;
    }
    close(prepared, &steps, failure, log_error).await
}

async fn run_step(
    engine: &drive::Engine,
    prepared: &Prepared<'_>,
    index: usize,
    id: uuid::Uuid,
    processed: &[uuid::Uuid],
    concurrency: usize,
) -> Result<(report::StepRecord, Option<Failure>), Failure> {
    let step = index.saturating_add(1);
    note(&format!("step {step} conversation {id}"));
    let before = engine.totals();
    let started = Instant::now();
    let done = drive::conversation(engine, id, concurrency).await;
    let llm = delta(before, engine.totals());
    let record = report::snapshot(&report::StepInput {
        dir: prepared.dir,
        index,
        conversation: id,
        processed,
        jobs: &done.jobs,
        llm,
        wall_ms: millis(started.elapsed()),
        pages_from: &prepared.dir.join("pages"),
        pool: engine.pool(),
        secrets: prepared.hidden,
    })
    .await?;
    Ok((record, done.failure))
}

fn delta(before: Totals, after: Totals) -> Totals {
    Totals {
        calls: after.calls.saturating_sub(before.calls),
        prompt_tokens: after.prompt_tokens.saturating_sub(before.prompt_tokens),
        completion_tokens: after
            .completion_tokens
            .saturating_sub(before.completion_tokens),
    }
}

async fn close(
    prepared: Prepared<'_>,
    steps: &[report::StepRecord],
    failure: Option<Failure>,
    log_error: Option<String>,
) -> Result<(), Failure> {
    let harness_error = failure
        .as_ref()
        .map(|error| failure::redact(&error.to_string(), prepared.hidden));
    let printed = report::write_run(&report::RunInput {
        dir: prepared.dir,
        label: &prepared.args.label,
        slice: &prepared.args.slice,
        model: &prepared.args.model,
        base_url: &prepared.args.base_url,
        concurrency: prepared.args.concurrency,
        started_at: prepared.started_at,
        wall_ms: millis(prepared.wall.elapsed()),
        setup_ms: prepared.setup_ms,
        steps,
        log_error,
        harness_error,
        carry: workers::entity_builder::fact_carry_report(),
        entities: workers::entity_builder::entity_run_report(),
    })
    .await?;
    emit(prepared.dir, &printed)?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn check(args: &Args) -> Result<workers::entity_builder::MatcherSettings, Failure> {
    if !slice::label_ok(&args.label) {
        return Err(Failure::Label {
            label: args.label.clone(),
        });
    }
    if args.concurrency == 0 {
        return Err(Failure::Concurrency);
    }
    drive::Workers::parse(&args.worker)?;
    workers::entity_builder::MatcherSettings::selected(&args.matcher, &args.matcher_url)
        .map_err(|detail| Failure::Usage { detail })
}

fn copied(counts: &copy::Counts, expected: usize) -> Result<(), Failure> {
    note(&format!(
        "copied {} conversations, {} messages, {} summaries, {} topics, {} aliases, {} placements",
        counts.conversations,
        counts.messages,
        counts.summaries,
        counts.topics,
        counts.aliases,
        counts.placements,
    ));
    if counts.conversations == 0 {
        return Err(Failure::EmptyCopy);
    }
    let Some(copied) = usize::try_from(counts.conversations).ok() else {
        return Err(Failure::PartialCopy {
            copied: counts.conversations,
            expected,
        });
    };
    if copied == expected {
        Ok(())
    } else {
        Err(Failure::PartialCopy {
            copied: counts.conversations,
            expected,
        })
    }
}

async fn prepare_dir(runs: &Path, label: &str) -> Result<PathBuf, Failure> {
    let root = absolute(runs).await?;
    let dir = root.join(format!("{}-{label}", Utc::now().format("%Y%m%dT%H%M%SZ")));
    if tokio::fs::try_exists(&dir)
        .await
        .map_err(|source| Failure::Write {
            path: dir.clone(),
            source,
        })?
    {
        return Err(Failure::Exists { path: dir });
    }
    tokio::fs::create_dir_all(dir.join("pages"))
        .await
        .map_err(|source| Failure::Write {
            path: dir.clone(),
            source,
        })?;
    tokio::fs::create_dir_all(dir.join("llm"))
        .await
        .map_err(|source| Failure::Write {
            path: dir.clone(),
            source,
        })?;
    absolute(&dir).await
}

async fn absolute(path: &Path) -> Result<PathBuf, Failure> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|source| Failure::Write {
            path: path.to_path_buf(),
            source,
        })?;
    tokio::fs::canonicalize(path)
        .await
        .map_err(|source| Failure::Write {
            path: path.to_path_buf(),
            source,
        })
}

async fn preflight(base: &str) -> Result<(), Failure> {
    let url = url::Url::parse(base).map_err(|error| Failure::Usage {
        detail: format!("base url: {error}"),
    })?;
    let host = url.host_str().ok_or_else(|| Failure::Usage {
        detail: "base url needs a host".to_owned(),
    })?;
    let port = url.port_or_known_default().ok_or_else(|| Failure::Usage {
        detail: "base url needs a port".to_owned(),
    })?;
    let mut addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| Failure::Model(error.to_string()))?;
    let address = addresses
        .next()
        .ok_or_else(|| Failure::Model(format!("{host}:{port} has no address")))?;
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(address),
    )
    .await
    .map_err(|_| Failure::Model(format!("{host}:{port} timed out")))?
    .map_err(|error| Failure::Model(error.to_string()))?;
    Ok(())
}

fn emit_step(step: &report::StepRecord) -> Result<(), Failure> {
    emit_line(&format!(
        "step {n} conversation {id} summaries {summaries} ok {ok} failed {failed} skipped {skipped} not_run {not_run} pages {pages} llm_calls {calls} prompt_tokens {prompt} completion_tokens {completion} wall_ms {wall} suffixed_slugs {slugs} collisions {collisions} extractions_not_done {pending} work {work} errors {errors}",
        n = step.step,
        id = step.conversation_id,
        summaries = step.summaries,
        ok = step.ok,
        failed = step.failed,
        skipped = step.skipped,
        not_run = step.not_run,
        pages = step.pages,
        calls = step.llm_calls,
        prompt = step.prompt_tokens,
        completion = step.completion_tokens,
        wall = step.wall_ms,
        slugs = step.suffixed_slugs,
        collisions = step.collisions,
        pending = step.extractions_not_done,
        work = step.work,
        errors = step.errors,
    ))
}

fn emit(dir: &Path, printed: &report::Printed) -> Result<(), Failure> {
    let lines = [
        format!("work {}", printed.work),
        format!("errors {}", printed.errors),
        format!("suffixed_slugs {}", printed.suffixed_slugs),
        format!("collisions {}", printed.collisions),
        format!("extractions_not_done {}", printed.extractions_not_done),
        format!("llm_calls {}", printed.llm_calls),
        format!("prompt_tokens {}", printed.prompt_tokens),
        format!("completion_tokens {}", printed.completion_tokens),
        format!(
            "rewrites_with_carry_forward {}",
            printed.rewrites_with_carry_forward
        ),
        format!("facts_carried {}", printed.facts_carried),
        format!("facts_retired {}", printed.facts_retired),
        format!("retirements_rejected {}", printed.retirements_rejected),
        format!("no_evidence_skips {}", printed.no_evidence_skips),
        format!("empty_rewrites {}", printed.empty_rewrites),
        format!("entity_errors {}", printed.entity_errors),
        format!("passage_chars {}", printed.passage_chars),
        format!("excerpted_messages {}", printed.excerpted_messages),
        format!("excerpt_ratio {}", printed.excerpt_ratio),
        format!("verified_refs {}", printed.verified_refs),
        format!("redirected_refs {}", printed.redirected_refs),
        format!("aliases_accepted {}", printed.aliases_accepted),
        format!("aliases_rejected {}", printed.aliases_rejected),
        format!("aliases_paired {}", printed.aliases_paired),
        format!("entity_merges {}", printed.merges),
        format!("run {}", dir.display()),
    ];
    for line in lines {
        emit_line(&line)?;
    }
    emit_examples(printed)
}

fn emit_examples(printed: &report::Printed) -> Result<(), Failure> {
    for example in &printed.carried_examples {
        emit_line(&format!(
            "carried_example {} {} {}",
            example.entity, example.attribute, example.detail
        ))?;
    }
    for example in &printed.retired_examples {
        emit_line(&format!(
            "retired_example {} {} {}",
            example.entity, example.attribute, example.detail
        ))?;
    }
    for example in &printed.rejected_examples {
        emit_line(&format!(
            "rejected_example {} {} {} {}",
            example.entity, example.attribute, example.kind, example.detail
        ))?;
    }
    for example in &printed.no_evidence_examples {
        emit_line(&format!(
            "no_evidence_example {} {}",
            example.entity, example.detail
        ))?;
    }
    for example in &printed.empty_rewrite_examples {
        emit_line(&format!(
            "empty_rewrite_example {} {}",
            example.entity, example.detail
        ))?;
    }
    for example in &printed.entity_error_examples {
        emit_line(&format!(
            "entity_error_example {} {}",
            example.entity, example.detail
        ))?;
    }
    for example in &printed.redirected_examples {
        emit_line(&format!(
            "redirected_example {} {}",
            example.entity, example.detail
        ))?;
    }
    for example in &printed.alias_rejected_examples {
        emit_line(&format!(
            "alias_rejected_example {} {}",
            example.entity, example.detail
        ))?;
    }
    for example in &printed.merge_examples {
        emit_line(&format!(
            "entity_merge_example {} -> {} {}",
            example.from_name, example.into_name, example.probability
        ))?;
    }
    Ok(())
}

fn emit_line(line: &str) -> Result<(), Failure> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}").map_err(|source| Failure::Report(source.to_string()))?;
    stdout
        .flush()
        .map_err(|source| Failure::Report(source.to_string()))
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}
