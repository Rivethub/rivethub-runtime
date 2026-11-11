use std::time::Duration;

use capture::backfill::Backfill;
use capture::backfill::HarnessTotals;
use capture::hook::ClaudeHook;
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
        CaptureCommand::Hook { harness } => {
            let mut stdin = String::new();
            tokio::io::stdin().read_to_string(&mut stdin).await?;
            if env.get(config::MODEL_RUN_VARIABLE).is_some() {
                return Ok(report(String::new()));
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
    let stderr = match writer_as(env, owner_uid).await {
        Ok((writer, paths)) => {
            ClaudeHook::new(writer, &paths)
                .run(stdin, now, &capture::hook::ingest_key(now))
                .await
                .stderr
        }
        Err(error) => format!("capture hook is not configured: {error}"),
    };
    report(stderr)
}

pub async fn store_hook(
    env: &Environment,
    stdin: &str,
    now: DateTime<Utc>,
    harness: StoreHarness,
) -> Report {
    let stderr = match writer(env).await {
        Ok((writer, paths)) => {
            match StoreHook::new(harness, writer, &capture::listing::store_roots(env), &paths) {
                Some(hook) => hook.run(stdin, now).await.stderr,
                None => format!("capture hook has no {harness:?} store: HOME is unset"),
            }
        }
        Err(error) => format!("capture hook is not configured: {error}"),
    };
    report(stderr)
}

pub(crate) async fn ingest_bundle(
    env: &Environment,
    bundle: &wire::capture::SessionBundle,
    receipt: Option<&wire::capture::Receipt>,
    now: DateTime<Utc>,
) -> Result<capture::writer::Totals, Error> {
    let (writer, _) = writer(env).await?;
    let mut totals = capture::writer::Totals {
        inserted: 0,
        skipped: 0,
        conversation_id: String::new(),
    };
    for session in &bundle.sessions {
        let batch = match receipt {
            Some(receipt) => session.received_batch(receipt)?,
            None => session.import_batch()?,
        };
        let written = writer.write(&batch, now).await?;
        totals.inserted += written.inserted;
        totals.skipped += written.skipped;
        totals.conversation_id = written.conversation_id;
    }
    Ok(totals)
}

pub async fn replay_spool(env: &Environment, now: DateTime<Utc>) -> Result<(), Error> {
    let (writer, _) = writer(env).await?;
    Ok(writer.replay(now).await?)
}

fn report(stderr: String) -> Report {
    Report {
        stdout: String::new(),
        stderr: if stderr.is_empty() {
            stderr
        } else {
            format!("{stderr}\n")
        },
        exit: Exit::Ok,
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
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use chrono::Utc;
    use config::Environment;

    const CONFIG: &str = "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n    api_key: secret\n";
    const TOOL: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;

    #[tokio::test]
    async fn a_squatter_receives_nothing_and_the_batch_is_spooled() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&received);
        let app = axum::Router::new().fallback(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { axum::http::StatusCode::OK }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let home = tempfile::tempdir().expect("home");
        let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
        let state = gateway::state_dir(&env).expect("state");
        std::fs::create_dir_all(&state).expect("state dir");
        let token = gateway::local_owner::OwnerToken::generate().expect("token");
        let path = gateway::local_owner::token_path(&state);
        std::fs::write(&path, token.expose()).expect("token file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::write(
            home.path().join(".rivethub/config.yaml"),
            format!("{CONFIG}gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        )
        .expect("config");
        let foreign = nix::unistd::getuid().as_raw().wrapping_add(1);
        let report = super::hook_as(&env, TOOL, Utc::now(), foreign).await;
        assert!(
            report.stderr.contains("not owned by this user"),
            "{}",
            report.stderr
        );
        let spooled = std::fs::read_dir(state.join("capture-spool")).map_or(0, Iterator::count);
        assert_eq!(spooled, 1, "the batch was lost");
        assert_eq!(received.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.abort();
    }
}
