use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use config::Environment;
use kernel::memory::summary_conversion::{
    ConversionControl, ConversionProgress, SummaryConversion,
};
use kernel::tenancy::Scope;
use memory_postgres::PostgresMemory;
use tokio_util::sync::CancellationToken;

use super::target::{Kind, Target, resolve};
use crate::argv::{Scan, Taken};
use crate::{Error, Report};

pub(super) struct Args {
    action: Option<ConversionControl>,
    rehearsal: Vec<uuid::Uuid>,
    sqlite: Option<PathBuf>,
    watch: bool,
    json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "kebab-case")]
enum Verb {
    Start,
    Pause,
    Resume,
    Status,
    Retry,
    Release,
    Rehearse,
}

pub(super) fn parse(args: &[String]) -> Result<Args, Error> {
    let (verb, options) = args.split_first().ok_or(Error::ResummarizeUsage)?;
    let verb = verb.parse::<Verb>();
    let verb = match verb {
        Ok(verb) => Some(verb),
        Err(error) => {
            tracing::debug!(%error, "unknown conversion command");
            None
        }
    };
    let (conversation, options) = if matches!(verb, Some(Verb::Retry | Verb::Release)) {
        let (id, options) = options.split_first().ok_or(Error::ResummarizeUsage)?;
        (
            Some(id.parse().map_err(|_| Error::ResummarizeUsage)?),
            options,
        )
    } else {
        (None, options)
    };
    let mut scan = Scan::new(options);
    let mut sqlite = None;
    let mut rate = None;
    let mut include_giants = false;
    let mut watch = false;
    let mut json = false;
    let mut rehearsal = Vec::new();
    while let Some(arg) = scan.current().map(str::to_owned) {
        if scan.flag("--json") {
            json = true;
        } else if verb == Some(Verb::Status) && scan.flag("--watch") {
            watch = true;
        } else if matches!(verb, Some(Verb::Start | Verb::Resume)) && scan.flag("--include-giants")
        {
            include_giants = true;
        } else if let Taken::Value(path) = scan.option("--sqlite", None) {
            sqlite = Some(PathBuf::from(path));
        } else if verb == Some(Verb::Start)
            && let Taken::Value(value) = scan.option("--rate", None)
        {
            let parsed = value.parse::<i64>().map_err(|_| Error::ResummarizeUsage)?;
            if parsed <= 0 {
                return Err(Error::ResummarizeUsage);
            }
            rate = Some(parsed);
        } else if verb == Some(Verb::Rehearse) && !arg.starts_with('-') {
            rehearsal.push(arg.parse().map_err(|_| Error::ResummarizeUsage)?);
            scan.bump();
        } else {
            return Err(Error::UnknownOption(arg));
        }
    }
    let action = match verb {
        Some(Verb::Start) => Some(ConversionControl::Start {
            rate,
            include_giants,
        }),
        Some(Verb::Pause) => Some(ConversionControl::Pause),
        Some(Verb::Resume) => Some(ConversionControl::Resume { include_giants }),
        Some(Verb::Retry) => Some(ConversionControl::Retry {
            conversation: conversation.ok_or(Error::ResummarizeUsage)?,
        }),
        Some(Verb::Release) => Some(ConversionControl::Release {
            conversation: conversation.ok_or(Error::ResummarizeUsage)?,
        }),
        Some(Verb::Rehearse)
            if !rehearsal.is_empty()
                && rehearsal.len() <= kernel::memory::summary_conversion::REHEARSAL_LIMIT =>
        {
            None
        }
        Some(Verb::Status) => None,
        Some(Verb::Rehearse) | None => return Err(Error::ResummarizeUsage),
    };
    Ok(Args {
        action,
        rehearsal,
        sqlite,
        watch,
        json,
    })
}

pub(super) async fn run(
    args: Args,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let target = resolve(env, args.sqlite, Kind::Either)?;
    if !args.rehearsal.is_empty() {
        return rehearse(target, args.rehearsal, args.json).await;
    }
    let store: Box<dyn SummaryConversion> = match target {
        Target::Postgres(url) => Box::new(PostgresMemory::connect(&url).await?),
        Target::Sqlite(path) => Box::new(super::run::open_sqlite(&path).await?),
    };
    Scope::Maintenance
        .run(async {
            let progress = if let Some(action) = args.action {
                store.control(action).await?
            } else {
                store.status().await?
            };
            if !args.watch {
                return Ok(Report::success(format(&progress, args.json)?));
            }
            write_snapshot(&format(&progress, args.json)?)?;
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Ok(Report::success("")),
                    () = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                let progress = store.status().await?;
                write_snapshot(&format(&progress, args.json)?)?;
            }
        })
        .await
}

fn write_snapshot(snapshot: &str) -> Result<(), std::io::Error> {
    let mut out = std::io::stdout().lock();
    out.write_all(snapshot.as_bytes())?;
    out.flush()
}

fn format(progress: &ConversionProgress, json: bool) -> Result<String, Error> {
    if json {
        return Ok(format!("{}\n", serde_json::to_string(progress)?));
    }
    let mode = if progress.enabled {
        "enabled"
    } else {
        "paused"
    };
    let inventory = if progress.inventory_complete {
        "complete"
    } else {
        "scanning"
    };
    let cursor = progress
        .inventory_after
        .map_or_else(|| "none".to_owned(), |id| id.to_string());
    let current = progress
        .current
        .map_or_else(|| "none".to_owned(), |id| id.to_string());
    let mut text = format!(
        "Conversion {mode} at {} input tokens/min\n\
         Inventory {inventory}, cursor {cursor}, {} discovered\n\
         {} pending, {} waiting for quiet, {} reserved, {} complete, {} failed, {} giants deferred\n\
         {} input tokens charged, {} provider input tokens, {} provider output tokens\n\
         Current conversation {current}\n",
        progress.rate,
        progress.discovered,
        progress.pending,
        progress.waiting_quiet,
        progress.reserved,
        progress.complete,
        progress.failed,
        progress.giant_deferred,
        progress.charged_tokens,
        progress.provider_input_tokens,
        progress.provider_output_tokens
    );
    for (id, error) in &progress.recent_failures {
        writeln!(text, "{id} {error}").map_err(std::io::Error::other)?;
    }
    Ok(text)
}

async fn rehearse(target: Target, ids: Vec<uuid::Uuid>, json: bool) -> Result<Report, Error> {
    let result = match target {
        Target::Sqlite(path) => {
            tokio::task::spawn_blocking(move || {
                memory_sqlite::SqliteStore::rehearse_conversion(&path, &ids)
            })
            .await??
        }
        Target::Postgres(url) => {
            let store = PostgresMemory::connect(&url).await?;
            Scope::Maintenance
                .run(store.rehearse_conversion(&ids))
                .await?
        }
    };
    if json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&result)?
        )));
    }
    let mut text = format!(
        "{} conversations, {}\n",
        result.sampled_conversations, result.plan
    );
    for (table, counts) in result.tables {
        writeln!(
            text,
            "{table}  {} remapped  {} deleted",
            counts.remapped, counts.deleted
        )
        .map_err(std::io::Error::other)?;
    }
    Ok(Report::success(text))
}
