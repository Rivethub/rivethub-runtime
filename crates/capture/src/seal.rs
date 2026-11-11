use std::fmt;
use std::io::Read as _;
use std::str::FromStr;

use age::secrecy::ExposeSecret as _;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use crate::bundle::BundleError;
use crate::sanitize::SanitizeError;
use crate::sign::BadSignature;
use crate::sign::NodeKey;
use crate::sign::PublicKey;
use crate::sign::Signature;
use crate::sign::TrustStore;
use crate::sign::Verification;

pub const MAX_BUNDLE_BYTES: usize = 64 << 20;
pub const MAX_PLAIN_BYTES: usize = 256 << 20;
pub const CODE_PREFIX: &str = "rhb1:";
const MAGIC: &[u8] = b"RHB1\n";
const ZSTD_LEVEL: i32 = 3;
const AGE_KEY_PREFIX: &str = "AGE-SECRET-KEY-1";
const DROP_ID_CHARS: usize = 22;

#[derive(Debug, Error)]
pub enum SealError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("the bundle is not UTF-8 text, so it is not a session bundle")]
    Text,
    #[error("the bundle is {bytes} bytes; a shared bundle holds at most {MAX_BUNDLE_BYTES}")]
    TooLarge { bytes: u64 },
    #[error(
        "the bundle is raw; run rh memory sanitize on it first, because only a sanitized bundle can be shared"
    )]
    Raw,
    #[error(
        "the bundle still withholds {0} flagged chunks; mark them in the report and run rh memory sanitize --resolve before you share it"
    )]
    Withheld(usize),
    #[error(
        "the bundle has {0} advisory flags nobody marked; read the report, then confirm the count with --flags {0}"
    )]
    Unconfirmed(usize),
    #[error(transparent)]
    Flags(#[from] SanitizeError),
    #[error("the envelope header could not be written: {0}")]
    Header(#[from] serde_json::Error),
    #[error("the bundle could not be compressed: {0}")]
    Compress(std::io::Error),
    #[error("the bundle could not be encrypted: {0}")]
    Encrypt(#[from] age::EncryptError),
}

#[derive(Debug, Error)]
pub enum OpenError {
    #[error(
        "the download does not open with this share code's key; the code is wrong or the ciphertext was altered"
    )]
    Decrypt,
    #[error("the download is not a RivetHub bundle envelope")]
    Envelope,
    #[error("the bundle decompresses past {MAX_PLAIN_BYTES} bytes, so it was refused")]
    TooLarge,
    #[error("the bundle is not UTF-8 text, so it is not a session bundle")]
    Text,
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(transparent)]
    Signature(#[from] BadSignature),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CodeError {
    #[error("a share code starts with {CODE_PREFIX}; check that you copied all of it")]
    Prefix,
    #[error("the share code has no #key part; check that you copied all of it")]
    NoKey,
    #[error(
        "the share code's bundle id is not 22 letters, digits, - or _; check that you copied all of it"
    )]
    Id,
    #[error(
        "the share code's key fails its checksum; a character is mistyped, and nothing was downloaded"
    )]
    Checksum,
}

pub struct ShareKey(age::x25519::Identity);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: the share key is the only secret in a share code; Debug must never print it"
    )
)]
impl fmt::Debug for ShareKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ShareKey(..)")
    }
}

impl ShareKey {
    #[must_use]
    pub fn generate() -> Self {
        Self(age::x25519::Identity::generate())
    }

    #[must_use]
    pub fn reveal(&self) -> String {
        let full = self.0.to_string();
        let full = full.expose_secret();
        full.strip_prefix(AGE_KEY_PREFIX)
            .unwrap_or(full)
            .to_ascii_lowercase()
    }
}

impl FromStr for ShareKey {
    type Err = CodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let full = format!("{AGE_KEY_PREFIX}{}", text.trim().to_ascii_uppercase());
        age::x25519::Identity::from_str(&full)
            .map(Self)
            .map_err(|_| CodeError::Checksum)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropId(String);

impl DropId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DropId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for DropId {
    type Err = CodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valid = text.len() == DROP_ID_CHARS
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(CodeError::Id)
        }
    }
}

#[derive(Debug)]
pub struct ShareCode {
    pub id: DropId,
    pub key: ShareKey,
}

impl ShareCode {
    #[must_use]
    pub fn reveal(&self) -> String {
        format!("{CODE_PREFIX}{}#{}", self.id, self.key.reveal())
    }
}

impl FromStr for ShareCode {
    type Err = CodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let rest = text
            .trim()
            .strip_prefix(CODE_PREFIX)
            .ok_or(CodeError::Prefix)?;
        let (id, key) = rest.split_once('#').ok_or(CodeError::NoKey)?;
        Ok(Self {
            id: id.parse()?,
            key: key.parse()?,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<Signature>,
}

#[derive(Debug)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub key: ShareKey,
    pub titles: Vec<String>,
    pub author: String,
    pub signed: Option<PublicKey>,
}

pub fn seal(
    text: &[u8],
    signer: Option<&NodeKey>,
    confirmed_flags: usize,
) -> Result<Sealed, SealError> {
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(SealError::TooLarge {
            bytes: text.len() as u64,
        });
    }
    let bundle = parse(text).map_err(|error| match error {
        OpenError::Bundle(error) => SealError::Bundle(error),
        OpenError::Text
        | OpenError::Decrypt
        | OpenError::Envelope
        | OpenError::TooLarge
        | OpenError::Signature(_) => SealError::Text,
    })?;
    if bundle.manifest.sanitization == Sanitization::Raw {
        return Err(SealError::Raw);
    }
    let flags = crate::sanitize::open_flags(&bundle)?;
    if flags.withheld > 0 {
        return Err(SealError::Withheld(flags.withheld));
    }
    if flags.unreviewed > 0 && flags.unreviewed != confirmed_flags {
        return Err(SealError::Unconfirmed(flags.unreviewed));
    }
    let signature = signer.map(|key| key.sign(text, &bundle.manifest.exported_by.user));
    let signer_key = signature.as_ref().map(|signature| signature.public_key);
    let titles = bundle
        .sessions
        .iter()
        .map(|session| {
            session
                .batch
                .title
                .clone()
                .unwrap_or_else(|| "(untitled)".to_owned())
        })
        .collect();
    let author = bundle.manifest.exported_by.user;
    drop(bundle.sessions);
    let header = serde_json::to_vec(&Header { signature })?;
    let mut plaintext = Vec::with_capacity(MAGIC.len() + header.len() + 1 + text.len() / 4);
    plaintext.extend_from_slice(MAGIC);
    plaintext.extend_from_slice(&header);
    plaintext.push(b'\n');
    zstd::stream::copy_encode(text, &mut plaintext, ZSTD_LEVEL).map_err(SealError::Compress)?;
    let key = ShareKey::generate();
    let ciphertext = age::encrypt(&key.0.to_public(), &plaintext)?;
    drop(plaintext);
    Ok(Sealed {
        ciphertext,
        key,
        titles,
        author,
        signed: signer_key,
    })
}

#[derive(Debug)]
pub struct Opened {
    pub bundle: SessionBundle,
    pub verification: Verification,
}

pub fn open(ciphertext: Vec<u8>, key: &ShareKey, trust: &TrustStore) -> Result<Opened, OpenError> {
    let plaintext = age::decrypt(&key.0, &ciphertext).map_err(|_| OpenError::Decrypt)?;
    drop(ciphertext);
    let rest = plaintext.strip_prefix(MAGIC).ok_or(OpenError::Envelope)?;
    let split = rest
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or(OpenError::Envelope)?;
    let (header, body) = rest.split_at(split);
    let header: Header = serde_json::from_slice(header).map_err(|_| OpenError::Envelope)?;
    let text = decompress(body.get(1..).unwrap_or_default())?;
    drop(plaintext);
    let bundle = parse(&text)?;
    let verification = trust.verification(
        header.signature.as_ref(),
        &text,
        &bundle.manifest.exported_by.user,
    )?;
    Ok(Opened {
        bundle,
        verification,
    })
}

fn decompress(body: &[u8]) -> Result<Vec<u8>, OpenError> {
    let decoder = zstd::stream::read::Decoder::new(body).map_err(|_| OpenError::Envelope)?;
    let mut text = Vec::new();
    decoder
        .take(MAX_PLAIN_BYTES as u64 + 1)
        .read_to_end(&mut text)
        .map_err(|_| OpenError::Envelope)?;
    if text.len() > MAX_PLAIN_BYTES {
        return Err(OpenError::TooLarge);
    }
    Ok(text)
}

fn parse(text: &[u8]) -> Result<SessionBundle, OpenError> {
    let text = std::str::from_utf8(text).map_err(|_| OpenError::Text)?;
    Ok(crate::bundle::parse(text)?)
}
