use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use super::FactError;

const KEY_MAX: usize = 64;
const VALUE_MAX: usize = 400;
const EVIDENCE_MAX: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(Uuid);

impl FactId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for FactId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey(String);

impl FactKey {
    pub fn new(text: &str) -> Result<Self, FactError> {
        let folded = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !folded.chars().any(char::is_alphanumeric) {
            return Err(FactError::EmptyKey);
        }
        bounded(folded, "key", KEY_MAX).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactValue(String);

impl FactValue {
    pub fn new(text: &str) -> Result<Self, FactError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(FactError::EmptyValue);
        }
        bounded(trimmed.to_owned(), "value", VALUE_MAX).map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence(String);

impl Evidence {
    #[must_use]
    pub fn quote(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self(trimmed.chars().take(EVIDENCE_MAX).collect()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn bounded(text: String, field: &'static str, max: usize) -> Result<String, FactError> {
    if text.chars().count() > max {
        return Err(FactError::TooLong { field, max });
    }
    Ok(text)
}
