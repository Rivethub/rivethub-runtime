use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use percent_encoding::percent_decode_str;
use thiserror::Error;

pub use values::{ImportKey, SessionId, SessionIdError, SessionKey, SessionKeyError};

const SEGMENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionRef {
    Canonical(SessionId),
    Bare(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSegment {
    pub requested: String,
    pub session: SessionRef,
}

impl DecodedSegment {
    #[must_use]
    pub fn redirected_to(&self, resolved: &SessionId) -> Option<SessionId> {
        (self.requested != resolved.as_str()).then(|| resolved.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid session id: {segment}")]
pub struct SegmentError {
    pub segment: String,
}

#[must_use]
pub fn encode_segment(session: &SessionId) -> String {
    SEGMENT.encode(session.as_str())
}

pub fn decode_segment(segment: &str) -> Result<DecodedSegment, SegmentError> {
    let decoded = percent_decode_str(segment).decode_utf8();
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: segment decode keeps the raw segment when percent-decoding fails"
        )
    )]
    let text: &str = match &decoded {
        Ok(text) => text,
        Err(_) => segment,
    };
    if let Some(canonical) = canonical_from_base64(text) {
        return Ok(DecodedSegment {
            session: SessionRef::Canonical(collapse_path_fallback(&canonical)),
            requested: canonical.into(),
        });
    }
    if is_uuid(text) {
        return Ok(DecodedSegment {
            requested: text.to_owned(),
            session: SessionRef::Bare(text.to_owned()),
        });
    }
    Err(SegmentError {
        segment: segment.to_owned(),
    })
}

fn canonical_from_base64(text: &str) -> Option<SessionId> {
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && let Ok(bytes) = SEGMENT.decode(text)
        && let Ok(decoded) = String::from_utf8(bytes)
        && let Ok(session) = decoded.parse()
    {
        return Some(session);
    }
    None
}

fn collapse_path_fallback(session: &SessionId) -> SessionId {
    if let Some((_, tail)) = session.native().rsplit_once('/')
        && is_uuid(tail)
    {
        return SessionId::new(session.harness(), tail);
    }
    session.clone()
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}
