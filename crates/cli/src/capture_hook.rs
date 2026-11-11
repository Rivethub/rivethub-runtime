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

    use capture::writer::PostError;
    use chrono::Utc;
    use config::Environment;
    use tokio::io::AsyncReadExt as _;

    const CONFIG: &str = "runtime:\n  workspace: /home/user/work\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1/v1\n    model: m\n    api_key: secret\n";
    const TOOL: &str = r#"{"session_id":"55555555-5555-5555-5555-555555555555","cwd":"/work/demo","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"toolu_1"}"#;

    enum Reply {
        None,
        Ok,
        Status(u16),
        Garbage,
        Hangup,
    }

    enum Token {
        Valid,
        Missing,
        Symlink,
        Shared,
        Fifo,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Delivered,
        Spooled,
        DeadLettered,
        Lost,
    }

    fn expected(error: &PostError) -> Outcome {
        match error {
            PostError::Unauthorized
            | PostError::Refused { .. }
            | PostError::Server { .. }
            | PostError::Network { .. } => Outcome::Spooled,
            PostError::Client { .. } => Outcome::DeadLettered,
        }
    }

    struct Row {
        name: &'static str,
        reply: Reply,
        token: Token,
        foreign: bool,
        error: Option<PostError>,
        needle: &'static str,
    }

    #[expect(clippy::too_many_lines, reason = "one table, one loop")]
    #[tokio::test]
    async fn a_capture_batch_is_delivered_or_kept_and_a_foreign_listener_sees_nothing() {
        let rows = [
            Row {
                name: "delivered",
                reply: Reply::Ok,
                token: Token::Valid,
                foreign: false,
                error: None,
                needle: "",
            },
            Row {
                name: "no token file",
                reply: Reply::Ok,
                token: Token::Missing,
                foreign: false,
                error: None,
                needle: "",
            },
            Row {
                name: "port closed",
                reply: Reply::None,
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Refused {
                    reason: String::new(),
                }),
                needle: "is not running",
            },
            Row {
                name: "foreign listener",
                reply: Reply::Ok,
                token: Token::Valid,
                foreign: true,
                error: Some(PostError::Refused {
                    reason: String::new(),
                }),
                needle: "not owned by this user",
            },
            Row {
                name: "symlinked token",
                reply: Reply::Ok,
                token: Token::Symlink,
                foreign: false,
                error: Some(PostError::Refused {
                    reason: String::new(),
                }),
                needle: "local owner token",
            },
            Row {
                name: "mode 0640 token",
                reply: Reply::Ok,
                token: Token::Shared,
                foreign: false,
                error: Some(PostError::Refused {
                    reason: String::new(),
                }),
                needle: "readable by other users",
            },
            Row {
                name: "fifo token",
                reply: Reply::Ok,
                token: Token::Fifo,
                foreign: false,
                error: Some(PostError::Refused {
                    reason: String::new(),
                }),
                needle: "local owner token",
            },
            Row {
                name: "401",
                reply: Reply::Status(401),
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Unauthorized),
                needle: "401",
            },
            Row {
                name: "400",
                reply: Reply::Status(400),
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Client { status: 400 }),
                needle: "HTTP 400",
            },
            Row {
                name: "500",
                reply: Reply::Status(500),
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Server { status: 500 }),
                needle: "HTTP 500",
            },
            Row {
                name: "connection dropped",
                reply: Reply::Hangup,
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Network {
                    message: String::new(),
                }),
                needle: "capture request failed",
            },
            Row {
                name: "200 with a garbage body",
                reply: Reply::Garbage,
                token: Token::Valid,
                foreign: false,
                error: Some(PostError::Network {
                    message: String::new(),
                }),
                needle: "capture request failed",
            },
        ];
        for Row {
            name,
            reply,
            token: token_file,
            foreign,
            error,
            needle,
        } in rows
        {
            let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("bind");
            let port = listener.local_addr().expect("addr").port();
            let server = match reply {
                Reply::None => {
                    drop(listener);
                    None
                }
                Reply::Hangup => {
                    let seen = std::sync::Arc::clone(&seen);
                    Some(tokio::spawn(async move {
                        loop {
                            let Ok((mut stream, _)) = listener.accept().await else {
                                break;
                            };
                            let mut buffer = vec![0; 65536];
                            let read = stream.read(&mut buffer).await.unwrap_or(0);
                            seen.lock().push(
                                String::from_utf8_lossy(buffer.get(..read).unwrap_or_default())
                                    .into_owned(),
                            );
                        }
                    }))
                }
                Reply::Ok | Reply::Status(_) | Reply::Garbage => {
                    let seen = std::sync::Arc::clone(&seen);
                    let canned = match reply {
                        Reply::Ok => (
                            axum::http::StatusCode::OK,
                            r#"{"ok":true,"conversation_id":"c","inserted":1,"skipped":0}"#,
                        ),
                        Reply::Garbage => (axum::http::StatusCode::OK, "not json"),
                        Reply::Status(code) => {
                            (axum::http::StatusCode::from_u16(code).expect("status"), "")
                        }
                        Reply::None | Reply::Hangup => unreachable!(),
                    };
                    let app = axum::Router::new().fallback(
                        move |headers: axum::http::HeaderMap, body: String| {
                            seen.lock().push(format!("{headers:?} {body}"));
                            async move { canned }
                        },
                    );
                    Some(tokio::spawn(async move {
                        axum::serve(listener, app).await.expect("serve");
                    }))
                }
            };
            let home = tempfile::tempdir().expect("home");
            let env = Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
            let state = gateway::state_dir(&env).expect("state");
            std::fs::create_dir_all(&state).expect("state dir");
            let path = gateway::local_owner::token_path(&state);
            let secret = gateway::local_owner::OwnerToken::generate().expect("token");
            let write = |mode| {
                std::fs::write(&path, secret.expose()).expect("token file");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                    .expect("chmod");
            };
            match token_file {
                Token::Valid => write(0o600),
                Token::Shared => write(0o640),
                Token::Missing => {}
                Token::Symlink => {
                    let elsewhere = home.path().join("elsewhere");
                    std::fs::write(&elsewhere, secret.expose()).expect("target");
                    std::os::unix::fs::symlink(&elsewhere, &path).expect("symlink");
                }
                Token::Fifo => {
                    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).expect("fifo");
                }
            }
            std::fs::write(
                home.path().join(".rivethub/config.yaml"),
                format!("{CONFIG}gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
            )
            .expect("config");
            let uid = nix::unistd::getuid().as_raw();
            let uid = if foreign { uid.wrapping_add(1) } else { uid };
            let report = super::hook_as(&env, TOOL, Utc::now(), uid).await;
            if let Some(server) = server {
                server.abort();
            }
            let files = |dir: std::path::PathBuf| {
                std::fs::read_dir(dir).map_or(0, |entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().is_file())
                        .count()
                })
            };
            let spool = state.join("capture-spool");
            let seen = seen.lock().clone();
            let observed = match (files(spool.clone()), files(spool.join("dead")), seen.len()) {
                (1, 0, _) => Outcome::Spooled,
                (0, 1, _) => Outcome::DeadLettered,
                (0, 0, 1) => Outcome::Delivered,
                _ => Outcome::Lost,
            };
            let want = error.as_ref().map_or(Outcome::Delivered, expected);
            assert_eq!(observed, want, "{name}: {}", report.stderr);
            assert!(report.stderr.contains(needle), "{name}: {}", report.stderr);
            let refused = matches!(error, Some(PostError::Refused { .. }));
            if refused {
                assert!(seen.is_empty(), "{name}: a request left the hook");
            }
            if refused || matches!(token_file, Token::Missing) {
                assert!(
                    seen.iter()
                        .all(|request| !request.contains(secret.expose())),
                    "{name}: the token was sent"
                );
            }
            if matches!(token_file, Token::Missing) {
                assert!(
                    seen.iter()
                        .all(|request| !request.contains("authorization")),
                    "{name}: an Authorization header was sent"
                );
            }
            if name == "delivered" {
                let bearer = format!("Bearer {}", secret.expose());
                assert!(
                    seen.first()
                        .is_some_and(|request| request.contains(&bearer)),
                    "{name}: the owner token is not sent"
                );
            }
            if refused {
                assert!(
                    seen.iter().all(|request| !request.contains("toolu_1")),
                    "{name}: the payload was sent"
                );
            }
        }
    }
}
