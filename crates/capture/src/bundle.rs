use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;
use wire::capture::BundleAuthor;
use wire::capture::BundleManifest;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::SESSION_BUNDLE_VERSION;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;
use wire::capture::SessionOrigin;
use wire::capture::UNKNOWN_ORIGIN;

const FIRST_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("session bundle is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session bundle version {found} is not supported")]
    Version { found: u32 },
    #[error(
        "session bundle digest does not match its sessions: the manifest says {declared}, the sessions hash to {computed}"
    )]
    Digest { declared: String, computed: String },
    #[error("session bundle manifest counts {declared} sessions but holds {found}")]
    Count { declared: usize, found: usize },
    #[error("version 1 session {session_key} has no dates and no messages to take them from")]
    Undated { session_key: String },
}

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

#[derive(Deserialize)]
struct FirstBundle {
    sessions: Vec<CaptureBatchRequest>,
}

pub fn parse(text: &str) -> Result<SessionBundle, BundleError> {
    match serde_json::from_str::<Versioned>(text)?.version {
        SESSION_BUNDLE_VERSION => {
            let bundle: SessionBundle = serde_json::from_str(text)?;
            check(&bundle)?;
            Ok(bundle)
        }
        FIRST_VERSION => upgrade(serde_json::from_str::<FirstBundle>(text)?),
        found => Err(BundleError::Version { found }),
    }
}

pub fn seal(
    exported_by: BundleAuthor,
    exported_at: DateTime<Utc>,
    sessions: Vec<BundledSession>,
) -> Result<SessionBundle, BundleError> {
    Ok(SessionBundle {
        version: SESSION_BUNDLE_VERSION,
        manifest: BundleManifest {
            exported_at,
            exported_by,
            sanitization: Sanitization::Raw,
            session_count: sessions.len(),
            sessions_sha256: sessions_sha256(&sessions)?,
        },
        sessions,
    })
}

pub fn sessions_sha256(sessions: &[BundledSession]) -> Result<String, BundleError> {
    Ok(hex::encode(Sha256::digest(wire::json::canonical_json(
        sessions,
    )?)))
}

fn check(bundle: &SessionBundle) -> Result<(), BundleError> {
    let found = bundle.sessions.len();
    if bundle.manifest.session_count != found {
        return Err(BundleError::Count {
            declared: bundle.manifest.session_count,
            found,
        });
    }
    let computed = sessions_sha256(&bundle.sessions)?;
    if computed != bundle.manifest.sessions_sha256 {
        return Err(BundleError::Digest {
            declared: bundle.manifest.sessions_sha256.clone(),
            computed,
        });
    }
    Ok(())
}

fn upgrade(first: FirstBundle) -> Result<SessionBundle, BundleError> {
    let sessions = first
        .sessions
        .into_iter()
        .map(inferred)
        .collect::<Result<Vec<_>, _>>()?;
    let exported_at = sessions
        .iter()
        .map(|session| session.origin.updated_at)
        .max()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    seal(
        BundleAuthor {
            user: UNKNOWN_ORIGIN.to_owned(),
            node: UNKNOWN_ORIGIN.to_owned(),
        },
        exported_at,
        sessions,
    )
}

fn inferred(batch: CaptureBatchRequest) -> Result<BundledSession, BundleError> {
    let sent = batch
        .messages
        .iter()
        .filter_map(|message| message.created_at);
    let (Some(created_at), Some(updated_at)) = (
        batch.created_at.or_else(|| sent.clone().min()),
        batch.updated_at.or_else(|| sent.max()),
    ) else {
        return Err(BundleError::Undated {
            session_key: batch.session_key,
        });
    };
    Ok(BundledSession {
        origin: SessionOrigin {
            author: UNKNOWN_ORIGIN.to_owned(),
            node: UNKNOWN_ORIGIN.to_owned(),
            conversation_id: UNKNOWN_ORIGIN.to_owned(),
            session_key: batch.session_key.clone(),
            agent: batch.agent.clone(),
            created_at,
            updated_at,
        },
        batch,
    })
}
