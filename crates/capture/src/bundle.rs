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
    #[error("canonical session bundle text does not open with its manifest")]
    Layout,
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

pub struct Sealed {
    pub bundle: SessionBundle,
    pub text: Vec<u8>,
}

pub fn seal(
    exported_by: BundleAuthor,
    exported_at: DateTime<Utc>,
    sessions: Vec<BundledSession>,
) -> Result<Sealed, BundleError> {
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
    let covered = covered_json(&bundle)?;
    let digest = hex::encode(Sha256::digest(&covered));
    let text = with_digest(&covered, &digest)?;
    bundle.manifest.bundle_sha256 = digest;
    Ok(Sealed { bundle, text })
}

const MANIFEST_OPEN: &[u8] = br#"{"manifest":{"#;

// Canonical JSON sorts keys, so the covered text opens with the manifest and `bundle_sha256` sorts before every other
// manifest key: inserting it there yields the canonical JSON of the whole sealed bundle without serializing it again.
fn with_digest(covered: &[u8], digest: &str) -> Result<Vec<u8>, BundleError> {
    let Some(rest) = covered.strip_prefix(MANIFEST_OPEN) else {
        return Err(BundleError::Layout);
    };
    let member = format!(r#""bundle_sha256":"{digest}","#);
    let mut text = Vec::with_capacity(covered.len() + member.len());
    text.extend_from_slice(MANIFEST_OPEN);
    text.extend_from_slice(member.as_bytes());
    text.extend_from_slice(rest);
    Ok(text)
}

#[derive(Serialize)]
struct Covered<'a> {
    manifest: CoveredManifest<'a>,
    sessions: &'a [BundledSession],
    version: u32,
}

#[derive(Serialize)]
struct CoveredManifest<'a> {
    exported_at: DateTime<Utc>,
    exported_by: &'a BundleAuthor,
    sanitization: &'a Sanitization,
    session_count: usize,
}

pub fn bundle_sha256(bundle: &SessionBundle) -> Result<String, BundleError> {
    Ok(hex::encode(Sha256::digest(covered_json(bundle)?)))
}

fn covered_json(bundle: &SessionBundle) -> Result<Vec<u8>, BundleError> {
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
    Ok(wire::json::canonical_json(&covered)?)
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
    Ok(seal(
        BundleAuthor {
            user: UNKNOWN_ORIGIN.to_owned(),
            node: UNKNOWN_ORIGIN.to_owned(),
        },
        exported_at,
        sessions,
    )?
    .bundle)
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
