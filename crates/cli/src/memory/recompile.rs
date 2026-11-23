use jiff::Timestamp;
use std::path::Path;

use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue};
use kernel::wiki::page::WikiSlug;
use kernel::wiki::recompile::RecompileWiki;
use memory_postgres::PostgresMemory;

use super::run::open_sqlite;
use super::target::{Kind, Target, load_for, resolve};
use crate::Error;
use crate::Report;
use crate::argv::{Scan, Taken};
use config::Environment;

const MIN_BYTES: u64 = 60 * 1024;
const PRIORITY: i16 = 5;

pub(super) struct RecompileArgs {
    min_bytes: u64,
    since: Timestamp,
    slugs: Vec<WikiSlug>,
    dry_run: bool,
    json: bool,
    sqlite: Option<std::path::PathBuf>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Planned {
    queued: bool,
    pages: Vec<PlannedPage>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlannedPage {
    slug: String,
    bytes: u64,
}

pub(super) fn parse(args: &[String]) -> Result<RecompileArgs, Error> {
    let mut scan = Scan::new(args);
    let mut parsed = RecompileArgs {
        min_bytes: MIN_BYTES,
        since: Timestamp::MIN,
        slugs: Vec::new(),
        dry_run: false,
        json: false,
        sqlite: None,
    };
    let mut since = None;
    while scan.current().is_some() {
        match scan.option("--min-bytes", None) {
            Taken::Value(value) => {
                parsed.min_bytes = value
                    .parse()
                    .map_err(|_| Error::RecompileBytes(value.clone()))?;
                continue;
            }
            Taken::Missing => return Err(Error::RecompileBytes(String::new())),
            Taken::Absent => {}
        }
        match scan.option("--since", None) {
            Taken::Value(value) => {
                since = Some(
                    wire::time::parse_rfc3339(&value)
                        .map_err(|_| Error::SinceInvalid(value.clone()))?,
                );
                continue;
            }
            Taken::Missing => return Err(Error::SinceRequired),
            Taken::Absent => {}
        }
        match scan.option("--slug", None) {
            Taken::Value(value) => {
                parsed.slugs.push(
                    WikiSlug::try_from(value.clone()).map_err(|_| Error::RecompileSlug(value))?,
                );
                continue;
            }
            Taken::Missing => return Err(Error::RecompileSlug(String::new())),
            Taken::Absent => {}
        }
        match scan.option("--sqlite", None) {
            Taken::Value(value) => {
                parsed.sqlite = Some(std::path::PathBuf::from(value));
                continue;
            }
            Taken::Missing => return Err(Error::UnknownOption("--sqlite".to_owned())),
            Taken::Absent => {}
        }
        if scan.flag("--dry-run") {
            parsed.dry_run = true;
            continue;
        }
        if scan.flag("--json") {
            parsed.json = true;
            continue;
        }
        return Err(match scan.current() {
            Some(token) if token.starts_with('-') => Error::UnknownOption(token.to_owned()),
            Some(token) => Error::Unexpected(token.to_owned()),
            None => Error::Unexpected(String::new()),
        });
    }
    parsed.since = since.ok_or(Error::SinceRequired)?;
    Ok(parsed)
}

pub(super) async fn run(args: RecompileArgs, env: &Environment) -> Result<Report, Error> {
    let named = args.sqlite.is_some();
    let target = resolve(env, args.sqlite.clone(), Kind::Either);
    let config = load_for(env, named)?;
    let directory = config
        .filter(|_| matches!(&target, Ok(Target::Sqlite(_))))
        .unwrap_or_default()
        .wiki_directory(env);
    let pages = planned(&directory.join("topics"), &args).await?;
    if !args.dry_run {
        match target? {
            Target::Postgres(url) => {
                let memory = PostgresMemory::connect(&url).await?;
                enqueue(&memory.queue().await?, &pages, args.since).await?;
            }
            Target::Sqlite(path) => enqueue(&open_sqlite(&path).await?, &pages, args.since).await?,
        }
    }
    let body = Planned {
        queued: !args.dry_run,
        pages,
    };
    if args.json {
        return Ok(Report::success(format!(
            "{}\n",
            serde_json::to_string(&body)?
        )));
    }
    let mut lines: Vec<String> = body
        .pages
        .iter()
        .map(|page| format!("{:>9} {}", page.bytes, page.slug))
        .collect();
    lines.push(if body.queued {
        format!(
            "queued recompile-wiki for {} page(s); the wiki-builder worker rewrites them",
            body.pages.len()
        )
    } else {
        format!("{} page(s) would be recompiled", body.pages.len())
    });
    let text = lines.join("\n") + "\n";
    Ok(Report::success(text))
}

async fn enqueue(
    queue: &impl JobQueue,
    pages: &[PlannedPage],
    since: Timestamp,
) -> Result<(), Error> {
    for page in pages {
        let slug = WikiSlug::try_from(page.slug.clone())
            .map_err(|_| Error::RecompileSlug(page.slug.clone()))?;
        queue
            .enqueue(Job {
                name: JobName::Fixed(FixedJob::RecompileWiki),
                payload: wire::JsonObject::from_serialize(&RecompileWiki { slug, since })?,
                key: Some(format!("wiki-recompile-{}", page.slug)),
                key_mode: JobKeyMode::Replace,
                priority: PRIORITY,
                run_at: None,
                max_attempts: None,
            })
            .await?;
    }
    Ok(())
}

async fn planned(topics: &Path, args: &RecompileArgs) -> Result<Vec<PlannedPage>, Error> {
    let mut pages = Vec::new();
    if args.slugs.is_empty() {
        let mut entries = tokio::fs::read_dir(topics).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(slug) = name.to_str().and_then(|name| name.strip_suffix(".md")) else {
                continue;
            };
            let metadata = entry.metadata().await?;
            if metadata.is_file()
                && metadata.len() >= args.min_bytes
                && WikiSlug::try_from(slug.to_owned()).is_ok()
            {
                pages.push(PlannedPage {
                    slug: slug.to_owned(),
                    bytes: metadata.len(),
                });
            }
        }
    } else {
        for slug in &args.slugs {
            let path = topics.join(format!("{}.md", slug.as_ref()));
            let bytes = tokio::fs::metadata(&path).await?.len();
            pages.push(PlannedPage {
                slug: slug.as_ref().to_owned(),
                bytes,
            });
        }
    }
    pages.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.slug.cmp(&right.slug))
    });
    Ok(pages)
}
