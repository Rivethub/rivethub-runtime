use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use kernel::wiki::consolidate::ConsolidateWiki;
use kernel::wiki::consolidate::WikiConsolidateStore as _;
use kernel::wiki::consolidate::consolidation_plan;
use memory_postgres::PostgresMemory;
use memory_postgres::queue::QueueHealth;
use memory_sqlite::SqliteStore;
use tokio_util::sync::CancellationToken;

use super::export::MemoryExporter;
use super::io;
use super::parse::MemoryCommand;
use super::target::Kind;
use super::target::Target;
use super::target::resolve;
use crate::Error;
use crate::Report;
use config::Environment;

pub(super) async fn run(
    command: MemoryCommand,
    env: &Environment,
    shutdown: &CancellationToken,
    stdout_is_terminal: bool,
) -> Result<Report, Error> {
    match command {
        MemoryCommand::QueueStatus { json } => queue_status(json, env).await,
        MemoryCommand::Retry {
            tasks,
            error,
            limit,
            dry_run,
            json,
        } => retry(tasks, error, limit, dry_run, json, env).await,
        MemoryCommand::Requeue {
            task,
            limit,
            clamped,
            dry_run,
            json,
        } => requeue(task, limit, clamped, dry_run, json, env).await,
        MemoryCommand::ToolSynth {
            limit,
            dry_run,
            json,
        } => tool_synth(limit, dry_run, json, env).await,
        MemoryCommand::Export { out, since, sqlite } => {
            export(out, since, sqlite, env, shutdown, stdout_is_terminal).await
        }
        MemoryCommand::Bundle {
            out,
            sessions,
            sqlite,
        } => super::bundle::share(out, sessions, sqlite, env).await,
        MemoryCommand::IngestBundle {
            path,
            force,
            dry_run,
        } => super::bundle::ingest(&path, force, dry_run, env).await,
        MemoryCommand::Sanitize {
            input,
            out,
            mapping,
        } => super::sanitize::run(&input, &out, mapping, env).await,
        MemoryCommand::Import {
            path,
            dry_run,
            sqlite,
        } => import(path, dry_run, sqlite, env, shutdown).await,
        MemoryCommand::Titles {
            dry_run,
            json,
            sqlite,
            model: None,
        } => titles(dry_run, json, sqlite, env).await,
        MemoryCommand::Titles {
            dry_run,
            json,
            sqlite,
            model: Some(model),
        } => match resolve(env, sqlite, Kind::Either)? {
            Target::Postgres(url) => {
                super::titles::backfill(&url, &model, dry_run, json, shutdown).await
            }
            Target::Sqlite(_) => Err(Error::TitlesSqlite),
        },
        MemoryCommand::Reembed { dry_run, sqlite } => reembed(dry_run, sqlite, env).await,
        MemoryCommand::MergeStore(args) => super::merge::run(args, env, shutdown).await,
        MemoryCommand::ConsolidateWiki {
            limit,
            dry_run,
            json,
        } => consolidate_wiki(limit, dry_run, json, env).await,
        MemoryCommand::Cleanup(args) => super::cleanup::run(args, env, shutdown).await,
        MemoryCommand::RecompileWiki(args) => super::recompile::run(args, env).await,
    }
}

async fn queue_status(json: bool, env: &Environment) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&postgres_url(env)?).await?;
    let rows = memory.queue_status().await?;
    if json {
        Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&rows)?
        )))
    } else {
        Ok(Report::success(human_health(&rows)))
    }
}

async fn retry(
    tasks: Vec<String>,
    error: Option<String>,
    limit: Option<u32>,
    dry_run: bool,
    json: bool,
    env: &Environment,
) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&postgres_url(env)?).await?;
    let reset = memory
        .retry_failed(&tasks, error.as_deref(), limit, dry_run)
        .await?;
    if json {
        Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&RetryBody { reset, dry_run })?
        )))
    } else {
        Ok(Report::success(format!("reset {reset} dead job(s)\n")))
    }
}

async fn requeue(
    task: kernel::queue::JobName,
    limit: u32,
    clamped: bool,
    dry_run: bool,
    json: bool,
    env: &Environment,
) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&postgres_url(env)?).await?;
    let requeued = memory.requeue_failed(&task, limit, dry_run).await?;
    if json {
        Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&RequeueBody {
                requeued,
                dry_run,
                limit,
            })?
        )))
    } else {
        Ok(Report::success(requeue_text(requeued, clamped)))
    }
}

async fn tool_synth(
    limit: Option<u32>,
    dry_run: bool,
    json: bool,
    env: &Environment,
) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&postgres_url(env)?).await?;
    let enqueued = memory.backfill_tool_synth(limit, dry_run).await?;
    if json {
        Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&SynthBody { enqueued, dry_run })?
        )))
    } else {
        Ok(Report::success(format!(
            "enqueued {enqueued} synthesize-tool-call job(s)\n"
        )))
    }
}

async fn consolidate_wiki(
    limit: Option<u32>,
    dry_run: bool,
    json: bool,
    env: &Environment,
) -> Result<Report, Error> {
    let memory = PostgresMemory::connect(&postgres_url(env)?).await?;
    let mut options: ConsolidateWiki = serde_json::from_str("{}")?;
    options.dry_run = dry_run;
    if let Some(limit) = limit {
        options.limit_clusters = limit;
    }
    let limit = options.limit_clusters;
    if !dry_run {
        memory
            .queue()
            .await?
            .enqueue(Job {
                name: JobName::Fixed(FixedJob::ConsolidateWiki),
                payload: wire::JsonObject::from_serialize(&options)?,
                key: Some("wiki-consolidate".to_owned()),
                key_mode: JobKeyMode::Replace,
                priority: 5,
                run_at: None,
                max_attempts: None,
            })
            .await?;
        return Ok(Report::success(if json {
            format!(
                "{}\n",
                serde_json::to_string(&QueuedBody {
                    queued: true,
                    limit_clusters: limit,
                })?
            )
        } else {
            format!(
                "queued consolidate-wiki for up to {limit} cluster(s); the wiki-builder worker folds them\n"
            )
        }));
    }
    let plan = consolidation_plan(&memory.wiki_topic_stats().await?, &options);
    if json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&PlanBody {
                dry_run: true,
                folds: plan
                    .into_iter()
                    .map(|fold| FoldBody {
                        cluster: fold.cluster,
                        parent: fold.parent,
                        children: fold.children,
                    })
                    .collect(),
            })?
        )));
    }
    let merged: usize = plan.iter().map(|fold| fold.children.len()).sum();
    let mut lines: Vec<String> = plan
        .iter()
        .map(|fold| format!("{} <- {}", fold.parent, fold.children.join(", ")))
        .collect();
    lines.push(format!(
        "{} fold(s) planned, {merged} page(s) would be merged (dry run)",
        plan.len()
    ));
    let text = format!("{}\n", lines.join("\n"));
    Ok(Report::success(text))
}

async fn export(
    out: Option<PathBuf>,
    since: Option<DateTime<Utc>>,
    sqlite: Option<PathBuf>,
    env: &Environment,
    shutdown: &CancellationToken,
    stdout_is_terminal: bool,
) -> Result<Report, Error> {
    let exporter = match resolve(env, sqlite, Kind::Either)? {
        Target::Postgres(url) => {
            MemoryExporter::postgres(Arc::new(PostgresMemory::connect(&url).await?), url)
        }
        Target::Sqlite(path) => MemoryExporter::sqlite(Arc::new(open_sqlite(&path).await?)),
    };
    let bytes = exporter.gzipped(since, shutdown).await?;
    publish(out, bytes, stdout_is_terminal).await
}

async fn import(
    path: PathBuf,
    dry_run: bool,
    sqlite: Option<PathBuf>,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let target = resolve(env, sqlite, Kind::Either)?;
    if let Target::Postgres(url) = &target
        && super::target::cloud_url(url)?
    {
        return Ok(io::cloud_refusal());
    }
    let keep_file = matches!(target, Target::Sqlite(_));
    let loaded = io::read_dump(&path, keep_file).await?;
    let header = io::preamble(&loaded.text)?;
    match target {
        Target::Postgres(url) => {
            import_postgres(&url, &loaded.text, dry_run, header, shutdown).await
        }
        Target::Sqlite(path) => import_sqlite(path, loaded, dry_run, header, shutdown).await,
    }
}

async fn titles(
    dry_run: bool,
    json: bool,
    sqlite: Option<PathBuf>,
    env: &Environment,
) -> Result<Report, Error> {
    let titled = match resolve(env, sqlite, Kind::Either)? {
        Target::Postgres(url) => {
            PostgresMemory::connect(&url)
                .await?
                .backfill_titles(dry_run)
                .await?
        }
        Target::Sqlite(path) => open_sqlite(&path).await?.backfill_titles(dry_run).await?,
    };
    if json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&TitlesBody { titled, dry_run })?
        )));
    }
    if dry_run {
        Ok(Report::success(format!(
            "would title {titled} conversation(s)\n"
        )))
    } else {
        Ok(Report::success(format!(
            "titled {titled} conversation(s)\n"
        )))
    }
}

async fn reembed(
    dry_run: bool,
    sqlite: Option<PathBuf>,
    env: &Environment,
) -> Result<Report, Error> {
    let Target::Sqlite(path) = resolve(env, sqlite, Kind::Either)? else {
        return Err(Error::ReembedSqlite);
    };
    let report = open_sqlite(&path).await?.reembed(!dry_run).await?;
    let (cleared, queued) = (report.cleared, report.queued);
    Ok(Report::success(if dry_run {
        format!("would clear {cleared} stored vector(s) and queue {queued} embed job(s)\n")
    } else {
        format!(
            "cleared {cleared} stored vector(s) and queued {queued} embed job(s); the next embedding records the configured model\n"
        )
    }))
}

async fn import_postgres(
    url: &str,
    text: &str,
    dry_run: bool,
    header: io::Header,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let origin = io::finish_origin(header, super::target::origin_for_url(url)?);
    let imported = PostgresMemory::connect(url)
        .await?
        .import_memory_dump(text, dry_run, shutdown)
        .await?;
    io::summary_report(origin, imported.rows, dry_run)
}

async fn import_sqlite(
    database: PathBuf,
    loaded: io::Loaded,
    dry_run: bool,
    header: io::Header,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let origin = io::finish_origin(header, super::target::sqlite_origin()?);
    let store = open_sqlite(&database).await?;
    let dump = loaded.dump_path();
    let report = if dry_run {
        store.preview_pg_dump(dump, shutdown.clone()).await?
    } else {
        store.import_pg_dump(dump, shutdown.clone()).await?
    };
    io::summary_report(origin, io::sqlite_rows(report.rows), dry_run)
}

async fn publish(out: Option<PathBuf>, bytes: Vec<u8>, tty: bool) -> Result<Report, Error> {
    if let Some(path) = out {
        let display = path.display().to_string();
        tokio::task::spawn_blocking(move || io::write_bytes(&path, &bytes)).await??;
        return Ok(Report::success(format!("wrote {display}\n")));
    }
    if tty {
        return Err(Error::GzipTty);
    }
    Ok(Report {
        stdout: String::new(),
        stderr: String::new(),
        exit: crate::Exit::Ok,
        stdout_raw: bytes,
    })
}

pub(super) fn postgres_url(env: &Environment) -> Result<String, Error> {
    match resolve(env, None, Kind::Postgres)? {
        Target::Postgres(url) => Ok(url),
        Target::Sqlite(_) => Err(Error::PostgresUrlRequired),
    }
}

pub(super) async fn open_sqlite(path: &Path) -> Result<SqliteStore, Error> {
    let path = path.to_path_buf();
    Ok(tokio::task::spawn_blocking(move || SqliteStore::open(path)).await??)
}

fn human_health(rows: &[QueueHealth]) -> String {
    let mut lines = Vec::new();
    for row in rows {
        let mut line = format!(
            "{} pending={} running={} scheduled={} dead={} recent_dead={}",
            row.task, row.pending, row.running, row.scheduled, row.dead, row.recent_dead
        );
        if let Some(minutes) = row.oldest_pending_minutes {
            line.push_str(" oldest_pending_minutes=");
            line.push_str(&minutes.to_string());
        }
        if let Some(error) = &row.last_error {
            line.push_str(" last_error=");
            line.push_str(error);
        }
        lines.push(line);
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn requeue_text(count: u64, clamped: bool) -> String {
    if clamped {
        format!("requeued {count} dead job(s) (limit 1000)\n")
    } else {
        format!("requeued {count} dead job(s)\n")
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedBody {
    queued: bool,
    limit_clusters: u32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanBody {
    dry_run: bool,
    folds: Vec<FoldBody>,
}

#[derive(serde::Serialize)]
struct FoldBody {
    cluster: String,
    parent: String,
    children: Vec<String>,
}

#[derive(serde::Serialize)]
struct RetryBody {
    reset: u64,
    dry_run: bool,
}

#[derive(serde::Serialize)]
struct RequeueBody {
    requeued: u64,
    dry_run: bool,
    limit: u32,
}

#[derive(serde::Serialize)]
struct SynthBody {
    enqueued: u64,
    dry_run: bool,
}

#[derive(serde::Serialize)]
struct TitlesBody {
    titled: u64,
    dry_run: bool,
}
