use std::collections::BTreeMap;
use std::time::Duration;

use capture::receive::Checked;
use capture::receive::Family;
use capture::seal::SealError;
use capture::writer::HttpCaptureTransport;
use capture::writer::PostError;
use capture::writer::WriteError;
use config::Environment;
use wire::gateway::users::TokenScope;

use super::share::ShareError;
use crate::Error;
use crate::share_server::Server;
use crate::share_server::ServerError;
use crate::share_server::ShareServers;

const PUSH_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub hub: String,
    pub user: String,
    pub sessions: usize,
    pub inserted: u64,
    pub skipped: u64,
}

pub async fn push_to_hub(
    env: &Environment,
    named: &str,
    text: &[u8],
    confirmed_flags: usize,
) -> Result<Pushed, Error> {
    let text = std::str::from_utf8(text).map_err(|_| ShareError::Seal(SealError::Text))?;
    let bundle = capture::bundle::parse(text)?;
    capture::seal::shareable(&bundle, confirmed_flags).map_err(ShareError::from)?;
    let checked = capture::receive::check(&bundle);
    if !checked.is_clean() {
        return Err(rescanned(&checked).into());
    }
    let servers = ShareServers::from_env(env).await?;
    let hub = servers.hub(named)?;
    let server = servers.connect(&hub.server, PUSH_TIMEOUT).await?;
    let member = server.member().await?;
    if member.may(TokenScope::MemoryImport) == Some(false) {
        return Err(ServerError::MissingScope {
            server: hub.server,
            scope: TokenScope::MemoryImport,
        }
        .into());
    }
    let transport =
        HttpCaptureTransport::new(server.endpoint("")?.to_string(), server.client().clone())
            .with_bearer(server.access_token(None).await?);
    let loaded = crate::db::load(env, None).await?;
    let redaction = capture::redaction::resolve(Some(loaded.config.capture_redaction()), env);
    let mut pushed = Pushed {
        hub: hub.to_string(),
        user: member.user,
        sessions: bundle.sessions.len(),
        inserted: 0,
        skipped: 0,
    };
    for session in &bundle.sessions {
        let batch = session.import_batch().map_err(WriteError::from)?;
        let totals = capture::writer::deliver(&transport, &batch, redaction.as_ref())
            .await
            .map_err(|error| refused(&server, error))?;
        pushed.inserted += totals.inserted;
        pushed.skipped += totals.skipped;
    }
    Ok(pushed)
}

fn rescanned(checked: &Checked) -> ShareError {
    let (secrets, injected): (BTreeMap<Family, u32>, BTreeMap<Family, u32>) = checked
        .counts()
        .into_iter()
        .partition(|(family, _)| *family == Family::Secret);
    let mut found = Vec::new();
    let mut advice = Vec::new();
    if !secrets.is_empty() {
        found.push(format!(
            "secret shapes ({})",
            capture::receive::summarize(&secrets)
        ));
        advice.push("sanitize the raw bundle again");
    }
    if !injected.is_empty() {
        found.push(format!(
            "prompt-injection patterns ({})",
            capture::receive::summarize(&injected)
        ));
        advice.push(
            "the sanitizer leaves prompt-injection text in place, so redact it with rh memory sanitize --session <id> --redact-term <id>=<term> --out <sanitized bundle>, or drop that session's tool outputs with --drop-tool-outputs <id>",
        );
    }
    ShareError::Rescanned {
        found: found.join(" and "),
        advice: advice.join("; "),
    }
}

fn refused(server: &Server, error: WriteError) -> Error {
    let WriteError::Post(post) = error else {
        return error.into();
    };
    let host = server.host().clone();
    match post {
        PostError::Unauthorized => ServerError::TokenRefused {
            server: host,
            status: 401,
        },
        PostError::Client { status: 403 } => ServerError::Forbidden {
            server: host,
            scope: TokenScope::MemoryImport,
        },
        PostError::Client { status: 404 } => ServerError::NoCaptureRoute(host),
        PostError::Client { status } => ServerError::Answered(status),
        PostError::Throttled {
            status,
            retry_after_seconds,
        } => ServerError::Unavailable {
            status,
            retry_after: retry_after_seconds.map(|seconds| Duration::from_secs(seconds.into())),
        },
        PostError::Server { status } => ServerError::Unavailable {
            status,
            retry_after: None,
        },
        PostError::Network { message } | PostError::Refused { reason: message } => {
            ServerError::Unreachable(message)
        }
    }
    .into()
}
