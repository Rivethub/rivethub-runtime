use std::time::Duration;

use capture::hook::ClaudeHook;
use capture::hook::store::StoreHarness;
use capture::hook::store::StoreHook;
use capture::spool::CaptureSpool;
use capture::writer::CaptureWriter;
use capture::writer::HttpCaptureTransport;
use chrono::DateTime;
use chrono::Utc;
use clap::Subcommand;
use clap::ValueEnum;
use config::Environment;
use tokio::io::AsyncReadExt as _;

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
    }
}

pub async fn hook(env: &Environment, stdin: &str, now: DateTime<Utc>) -> Report {
    let stderr = match writer(env).await {
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

async fn writer(
    env: &Environment,
) -> Result<(CaptureWriter<HttpCaptureTransport>, config::Paths), Error> {
    let loaded = db::load(env, None).await?;
    let paths = config::Paths::new(env)?;
    let remote = loaded
        .config
        .memory
        .as_ref()
        .and_then(|memory| memory.capture.gateway.as_ref());
    let Endpoint { url, client } = match remote {
        Some(remote) => remote_gateway(remote).await?,
        None => status::gateway(env, &loaded.config, POST_TIMEOUT).await?,
    };
    let redaction = capture::redaction::resolve(Some(loaded.config.capture_redaction()), env);
    let writer = CaptureWriter::new(
        HttpCaptureTransport::new(url.as_str().trim_end_matches('/').to_owned(), client),
        CaptureSpool::new(paths.state_dir().join(SPOOL_DIR)),
        None,
        redaction,
    );
    Ok((writer, paths))
}

async fn remote_gateway(remote: &config::model::CaptureGateway) -> Result<Endpoint, Error> {
    let roots = reqwest::Certificate::from_pem_bundle(&tokio::fs::read(&remote.ca).await?)?;
    let mut identity = tokio::fs::read(&remote.key).await?;
    identity.push(b'\n');
    identity.extend(tokio::fs::read(&remote.cert).await?);
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .timeout(POST_TIMEOUT)
        .tls_certs_only(roots)
        .identity(reqwest::Identity::from_pem(&identity)?)
        .build()?;
    Ok(Endpoint {
        url: url::Url::parse(&remote.url)?,
        client,
    })
}
