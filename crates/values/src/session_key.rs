use crate::SessionId;
use crate::SessionIdError;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display};
use std::str::FromStr;
use thiserror::Error;

pub const IMPORTED_SESSION_PREFIX: &str = "import:";

const IMPORT_DIGEST_LEN: usize = 64;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct ImportKey(String);

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub enum SessionKey {
    Harness(SessionId),
    Import(ImportKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionKeyError {
    #[error(
        "invalid session key: {0} (an import key is import: followed by 64 lowercase hex digits)"
    )]
    Import(String),
    #[error(transparent)]
    Harness(#[from] SessionIdError),
}

impl ImportKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ImportKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ImportKey {
    type Err = SessionKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digest = value.strip_prefix(IMPORTED_SESSION_PREFIX);
        let well_formed = digest.is_some_and(|digest| {
            digest.len() == IMPORT_DIGEST_LEN
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if well_formed {
            Ok(Self(value.to_owned()))
        } else {
            Err(SessionKeyError::Import(value.to_owned()))
        }
    }
}

impl TryFrom<String> for ImportKey {
    type Error = SessionKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<ImportKey> for String {
    fn from(value: ImportKey) -> Self {
        value.0
    }
}

impl SessionKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Harness(session) => session.as_str(),
            Self::Import(key) => key.as_str(),
        }
    }

    #[must_use]
    pub fn session(&self) -> Option<&SessionId> {
        match self {
            Self::Harness(session) => Some(session),
            Self::Import(_) => None,
        }
    }

    #[must_use]
    pub fn harness(&self) -> Option<crate::HarnessId> {
        self.session().map(SessionId::harness)
    }
}

impl Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SessionKey {
    type Err = SessionKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.starts_with(IMPORTED_SESSION_PREFIX) {
            ImportKey::from_str(value).map(Self::Import)
        } else {
            Ok(Self::Harness(SessionId::from_str(value)?))
        }
    }
}

impl From<SessionId> for SessionKey {
    fn from(value: SessionId) -> Self {
        Self::Harness(value)
    }
}

impl From<&SessionId> for SessionKey {
    fn from(value: &SessionId) -> Self {
        Self::Harness(value.clone())
    }
}

impl From<ImportKey> for SessionKey {
    fn from(value: ImportKey) -> Self {
        Self::Import(value)
    }
}

impl TryFrom<String> for SessionKey {
    type Error = SessionKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<SessionKey> for String {
    fn from(value: SessionKey) -> Self {
        match value {
            SessionKey::Harness(session) => session.into(),
            SessionKey::Import(key) => key.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HarnessId;

    const DIGEST: &str = "9f2c0000000000000000000000000000000000000000000000000000000000e1";

    fn import_text() -> String {
        format!("{IMPORTED_SESSION_PREFIX}{DIGEST}")
    }

    #[test]
    fn an_import_key_is_the_prefix_and_sixty_four_lowercase_hex_digits()
    -> Result<(), SessionKeyError> {
        let key = import_text().parse::<SessionKey>()?;
        assert!(matches!(key, SessionKey::Import(_)));
        assert_eq!(key.as_str(), import_text());
        assert_eq!(key.harness(), None);
        for refused in [
            format!("import:{}", &DIGEST[1..]),
            format!("import:{DIGEST}0"),
            format!("import:{}", DIGEST.to_uppercase()),
            format!("import:{}z", &DIGEST[1..]),
            "import:".to_owned(),
            "import:xyz".to_owned(),
        ] {
            assert_eq!(
                refused.parse::<SessionKey>(),
                Err(SessionKeyError::Import(refused.clone())),
                "{refused}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_harness_key_parses_as_a_session_id() -> Result<(), SessionKeyError> {
        let key = "claude-code:abc".parse::<SessionKey>()?;
        assert_eq!(
            key,
            SessionKey::Harness(SessionId::new(HarnessId::ClaudeCode, "abc"))
        );
        assert_eq!(key.harness(), Some(HarnessId::ClaudeCode));
        assert!(matches!(
            "nothing".parse::<SessionKey>(),
            Err(SessionKeyError::Harness(_))
        ));
        Ok(())
    }

    #[test]
    fn a_key_round_trips_through_json_as_its_text() -> Result<(), serde_json::Error> {
        let text = serde_json::to_string(&import_text())?;
        let key: SessionKey = serde_json::from_str(&text)?;
        assert_eq!(serde_json::to_string(&key)?, text);
        assert!(serde_json::from_str::<SessionKey>("\"import:short\"").is_err());
        Ok(())
    }
}
