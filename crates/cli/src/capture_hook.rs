use std::time::Duration;

use capture::backfill::Backfill;
use capture::backfill::HarnessTotals;
use capture::hook::ClaudeHook;
use capture::hook::HookRun;
use capture::hook::store::StoreHarness;
use capture::hook::store::StoreHook;
use capture::spool::CaptureSpool;
use capture::writer::CaptureTransport;
use capture::writer::CaptureWriter;
use capture::writer::HttpCaptureTransport;
use capture::writer::PostError;
use chrono::DateTime;
use chrono::Utc;
use clap::Subcommand;
use clap::ValueEnum;
use config::Environment;
use tokio::io::AsyncReadExt as _;
use wire::harness::HarnessId;

use crate::status::Endpoint;
use crate::{Error, Exit, Report, db, status};

const SPOOL_DIR: &str = "capture-spool";
const POST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Subcommand)]
pub enum CaptureCommand {
    #[command(about = "Capture one harness hook payload read from stdin")]
    Hook {
        #[arg(long, value_enum, default_value_t = HookHarness::ClaudeCode)]
        harness: HookHarness,
    },
    #[command(about = "Show the capture spool, or move dead-lettered batches back to it")]
    Spool {
        #[arg(long)]
        requeue_dead: bool,
    },
    #[command(
        about = "Capture every stored session of the harness stores, Claude Code subagents included"
    )]
    Backfill {
        #[arg(long = "harness", value_enum)]
        harnesses: Vec<BackfillHarness>,
        #[arg(long, help = "Read and count the sessions and post nothing")]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackfillHarness {
    ClaudeCode,
    GrokBuild,
    Opencode,
    Codex,
}

impl BackfillHarness {
    fn id(self) -> HarnessId {
        match self {
            Self::ClaudeCode => HarnessId::ClaudeCode,
            Self::GrokBuild => HarnessId::GrokBuild,
            Self::Opencode => HarnessId::Opencode,
            Self::Codex => HarnessId::Codex,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookHarness {
    ClaudeCode,
    GrokBuild,
    Opencode,
    Codex,
    Dsh,
}

pub(crate) async fn run(command: CaptureCommand, env: &Environment) -> Result<Report, Error> {
    match command {
        CaptureCommand::Spool { requeue_dead } => spool_command(env, requeue_dead).await,
        CaptureCommand::Hook { harness } => {
            let mut stdin = String::new();
            tokio::io::stdin().read_to_string(&mut stdin).await?;
            if env.get(config::MODEL_RUN_VARIABLE).is_some() {
                return Ok(report(HookRun::done()));
            }
            Ok(match harness {
                HookHarness::ClaudeCode => hook(env, &stdin, Utc::now()).await,
                HookHarness::GrokBuild => {
                    store_hook(env, &stdin, Utc::now(), StoreHarness::GrokBuild).await
                }
                HookHarness::Opencode => {
                    store_hook(env, &stdin, Utc::now(), StoreHarness::Opencode).await
                }
                HookHarness::Codex => {
                    store_hook(env, &stdin, Utc::now(), StoreHarness::Codex).await
                }
                HookHarness::Dsh => store_hook(env, &stdin, Utc::now(), StoreHarness::Dsh).await,
            })
        }
        CaptureCommand::Backfill {
            harnesses,
            dry_run,
            json,
        } => {
            let ids: Vec<HarnessId> = harnesses.into_iter().map(BackfillHarness::id).collect();
            Ok(backfill(env, &ids, dry_run, json, Utc::now()).await)
        }
    }
}

pub async fn backfill(
    env: &Environment,
    harnesses: &[HarnessId],
    dry_run: bool,
    json: bool,
    now: DateTime<Utc>,
) -> Report {
    let (writer, _) = match writer(env).await {
        Ok(found) => found,
        Err(error) => return Report::failure(&error),
    };
    let wanted = if harnesses.is_empty() {
        &capture::backfill::HARNESSES[..]
    } else {
        harnesses
    };
    let totals = Backfill::new(writer, capture::listing::store_roots(env))
        .run(wanted, dry_run, now)
        .await;
    let failed = totals.iter().any(|found| !found.failures.is_empty());
    let (stdout, stderr) = if json {
        match serde_json::to_string(&BackfillJson {
            dry_run,
            harnesses: &totals,
        }) {
            Ok(text) => (format!("{text}\n"), String::new()),
            Err(error) => return Report::failure(&error),
        }
    } else {
        (summary(&totals, dry_run), failure_lines(&totals))
    };
    Report {
        stdout,
        stderr,
        exit: if failed { Exit::Failure } else { Exit::Ok },
        stdout_raw: Vec::new(),
    }
}

#[derive(serde::Serialize)]
struct BackfillJson<'a> {
    dry_run: bool,
    harnesses: &'a [HarnessTotals],
}

fn summary(totals: &[HarnessTotals], dry_run: bool) -> String {
    totals
        .iter()
        .map(|found| {
            let messages = if dry_run {
                format!("{} messages read, none posted", found.messages)
            } else {
                format!("{} messages added", found.added)
            };
            format!(
                "{}: {} sessions, {messages}, {} failures\n",
                found.harness,
                found.sessions,
                found.failures.len()
            )
        })
        .collect::<Vec<String>>()
        .concat()
}

fn failure_lines(totals: &[HarnessTotals]) -> String {
    totals
        .iter()
        .flat_map(|found| {
            found.failures.iter().map(move |failure| {
                format!("{} {}: {}\n", found.harness, failure.session, failure.error)
            })
        })
        .collect()
}

pub async fn hook(env: &Environment, stdin: &str, now: DateTime<Utc>) -> Report {
    hook_as(env, stdin, now, nix::unistd::getuid().as_raw()).await
}

async fn hook_as(env: &Environment, stdin: &str, now: DateTime<Utc>, owner_uid: u32) -> Report {
    match writer_as(env, owner_uid).await {
        Ok((writer, paths)) => report(
            ClaudeHook::new(writer, &paths)
                .run(stdin, now, &capture::hook::ingest_key(now))
                .await,
        ),
        Err(error) => report(HookRun::failed(format!(
            "capture hook is not configured: {error}"
        ))),
    }
}

pub async fn store_hook(
    env: &Environment,
    stdin: &str,
    now: DateTime<Utc>,
    harness: StoreHarness,
) -> Report {
    match writer(env).await {
        Ok((writer, paths)) => {
            match StoreHook::new(harness, writer, &capture::listing::store_roots(env), &paths) {
                Some(hook) => report(hook.run(stdin, now).await),
                None => report(HookRun::failed(format!(
                    "capture hook has no {harness:?} store: HOME is unset"
                ))),
            }
        }
        Err(error) => report(HookRun::failed(format!(
            "capture hook is not configured: {error}"
        ))),
    }
}

pub(crate) async fn ingest_bundle(
    env: &Environment,
    imports: &[(
        &wire::capture::BundledSession,
        Option<wire::capture::ReceiptChecks>,
    )],
    receipt: Option<&wire::capture::Receipt>,
    now: DateTime<Utc>,
) -> Result<capture::writer::Totals, Error> {
    let (writer, _) = writer(env).await?;
    let mut totals = capture::writer::Totals {
        inserted: 0,
        skipped: 0,
        conversation_id: String::new(),
    };
    for (session, checks) in imports {
        let batch = match receipt {
            Some(receipt) => session.received_batch(&wire::capture::Receipt {
                checks: checks.clone(),
                ..receipt.clone()
            })?,
            None => session.import_batch()?,
        };
        let written = writer.write(&batch, now).await?;
        totals.inserted += written.inserted;
        totals.skipped += written.skipped;
        totals.conversation_id = written.conversation_id;
    }
    Ok(totals)
}

fn spool_at(env: &Environment) -> Result<CaptureSpool, Error> {
    Ok(CaptureSpool::new(
        config::Paths::new(env)?.state_dir().join(SPOOL_DIR),
    ))
}

async fn spool_command(env: &Environment, requeue_dead: bool) -> Result<Report, Error> {
    let spool = spool_at(env)?;
    let moved = if requeue_dead {
        Some(spool.requeue_dead().await?)
    } else {
        None
    };
    let usage = spool.usage().await?;
    let mut text = format!(
        "{} batches are waiting to be delivered and {} are dead-lettered in {}.\n",
        usage.entries - usage.dead,
        usage.dead,
        spool.dir().display()
    );
    if let Some(moved) = moved {
        text = format!("{text}Moved {moved} dead-lettered batches back to the spool.\n");
    }
    Ok(Report::success(text))
}

pub(crate) async fn spool_warning(env: &Environment) -> Result<Option<String>, Error> {
    let usage = spool_at(env)?.usage().await?;
    let mut lines = Vec::new();
    if usage.over_threshold() {
        lines.push(format!(
            "The capture spool holds {} entries ({} MB). Nothing is dropped, but the gateway is not taking them. Start it with `rh local --ensure`.",
            usage.entries,
            usage.bytes / (1024 * 1024)
        ));
    }
    if usage.dead > 0 {
        lines.push(format!(
            "{} capture batches were dead-lettered. Run `rh capture spool --requeue-dead` to retry them.",
            usage.dead
        ));
    }
    Ok((!lines.is_empty()).then(|| lines.join("\n")))
}

pub async fn replay_spool(env: &Environment, now: DateTime<Utc>) -> Result<(), Error> {
    let (writer, _) = writer(env).await?;
    Ok(writer.replay(now).await?)
}

fn report(run: HookRun) -> Report {
    let stderr = run.stderr;
    Report {
        stdout: String::new(),
        stderr: if stderr.is_empty() {
            stderr
        } else {
            format!("{stderr}\n")
        },
        exit: if run.exit == 0 {
            Exit::Ok
        } else {
            Exit::Failure
        },
        stdout_raw: Vec::new(),
    }
}

async fn writer(env: &Environment) -> Result<(CaptureWriter<HookTransport>, config::Paths), Error> {
    writer_as(env, nix::unistd::getuid().as_raw()).await
}

async fn writer_as(
    env: &Environment,
    owner_uid: u32,
) -> Result<(CaptureWriter<HookTransport>, config::Paths), Error> {
    let loaded = db::load(env, None).await?;
    let paths = config::Paths::new(env)?;
    let remote = loaded
        .config
        .memory
        .as_ref()
        .and_then(|memory| memory.capture.gateway.as_ref());
    let endpoint = match remote {
        Some(remote) => remote_gateway(remote).await,
        None => status::gateway_as(env, &loaded.config, POST_TIMEOUT, Some(owner_uid)).await,
    };
    let transport = match endpoint {
        Ok(Endpoint { url, client }) => HookTransport::Http(HttpCaptureTransport::new(
            url.as_str().trim_end_matches('/').to_owned(),
            client,
        )),
        Err(error) => HookTransport::Refused(error.to_string()),
    };
    let redaction = capture::redaction::resolve(Some(loaded.config.capture_redaction()), env);
    let writer = CaptureWriter::new(
        transport,
        CaptureSpool::new(paths.state_dir().join(SPOOL_DIR)),
        None,
        redaction,
    );
    Ok((writer, paths))
}

#[derive(Debug)]
pub(crate) enum HookTransport {
    Http(HttpCaptureTransport),
    Refused(String),
}

#[async_trait::async_trait]
impl CaptureTransport for HookTransport {
    async fn post(&self, body: String) -> Result<wire::capture::CaptureResult, PostError> {
        match self {
            Self::Http(transport) => transport.post(body).await,
            Self::Refused(reason) => Err(PostError::Refused {
                reason: reason.clone(),
            }),
        }
    }
}

async fn remote_gateway(remote: &config::model::CaptureGateway) -> Result<Endpoint, Error> {
    let roots = reqwest::Certificate::from_pem_bundle(&tokio::fs::read(&remote.ca).await?)?;
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = &remote.token {
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::CaptureToken)?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .timeout(POST_TIMEOUT)
        .tls_certs_only(roots)
        .default_headers(headers)
        .build()?;
    Ok(Endpoint {
        url: url::Url::parse(&remote.url)?,
        client,
    })
}

#[cfg(test)]
mod tests;
