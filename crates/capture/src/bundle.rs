use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
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
        "session bundle digest does not match its contents: the manifest says {declared}, the bundle hashes to {computed}"
    )]
    Digest { declared: String, computed: String },
    #[error("session bundle manifest counts {declared} sessions but holds {found}")]
    Count { declared: usize, found: usize },
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
    let mut bundle = SessionBundle {
        version: SESSION_BUNDLE_VERSION,
        manifest: BundleManifest {
            exported_at,
            exported_by,
            sanitization: Sanitization::Raw,
            session_count: sessions.len(),
            bundle_sha256: String::new(),
        },
        sessions,
    };
    bundle.manifest.bundle_sha256 = bundle_sha256(&bundle)?;
    Ok(bundle)
}

#[derive(Serialize)]
struct Covered<'a> {
    version: u32,
    manifest: CoveredManifest<'a>,
    sessions: &'a [BundledSession],
}

#[derive(Serialize)]
struct CoveredManifest<'a> {
    exported_at: DateTime<Utc>,
    exported_by: &'a BundleAuthor,
    sanitization: &'a Sanitization,
    session_count: usize,
}

pub fn bundle_sha256(bundle: &SessionBundle) -> Result<String, BundleError> {
    let BundleManifest {
        exported_at,
        exported_by,
        sanitization,
        session_count,
        bundle_sha256: _,
    } = &bundle.manifest;
    let covered = Covered {
        version: bundle.version,
        manifest: CoveredManifest {
            exported_at: *exported_at,
            exported_by,
            sanitization,
            session_count: *session_count,
        },
        sessions: &bundle.sessions,
    };
    Ok(hex::encode(Sha256::digest(wire::json::canonical_json(
        &covered,
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
    let computed = bundle_sha256(bundle)?;
    if computed != bundle.manifest.bundle_sha256 {
        return Err(BundleError::Digest {
            declared: bundle.manifest.bundle_sha256.clone(),
            computed,
        });
    }
    Ok(())
}

fn upgrade(first: FirstBundle) -> Result<SessionBundle, BundleError> {
    let sessions: Vec<BundledSession> = first.sessions.into_iter().map(inferred).collect();
    let exported_at = sessions
        .iter()
        .filter_map(|session| session.origin.updated_at)
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

fn inferred(batch: CaptureBatchRequest) -> BundledSession {
    let sent = batch
        .messages
        .iter()
        .filter_map(|message| message.created_at);
    let created_at = batch.created_at.or_else(|| sent.clone().min());
    let updated_at = batch.updated_at.or_else(|| sent.max());
    BundledSession {
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
    }
}
