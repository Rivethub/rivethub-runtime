use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;

use super::super::key::LocalKey;
use super::places::Place;
use super::slots::BaseValue;
use super::slots::FieldAt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalReview {
    pub at: DateTime<Utc>,
    pub unreviewed_sha256: String,
    pub flags: Vec<LocalFlag>,
    pub fields: Vec<BaseField>,
    pub seal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFlag {
    pub number: usize,
    pub text: String,
    pub places: Vec<Place>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseField {
    pub at: FieldAt,
    pub base: BaseValue,
}

impl LocalReview {
    pub(super) fn sealed(mut self, key: &LocalKey) -> Result<Self, serde_json::Error> {
        self.seal = self.mac(key)?;
        Ok(self)
    }

    fn mac(&self, key: &LocalKey) -> Result<String, serde_json::Error> {
        let unsealed = Self {
            seal: String::new(),
            ..self.clone()
        };
        Ok(key.hex(&[b"local review", &wire::json::canonical_json(&unsealed)?]))
    }

    #[must_use]
    pub fn holds_its_seal(&self, key: &LocalKey) -> bool {
        self.mac(key).is_ok_and(|mac| mac == self.seal)
    }

    pub(super) fn bases(&self) -> BTreeMap<FieldAt, BaseValue> {
        self.fields
            .iter()
            .map(|field| (field.at.clone(), field.base.clone()))
            .collect()
    }

    pub(crate) fn holds(
        &self,
        sessions: &[BundledSession],
        by: &BundleAuthor,
    ) -> Result<bool, serde_json::Error> {
        let mut unreviewed = sessions.to_vec();
        super::render::restore(&mut unreviewed, &self.bases());
        Ok(digest(&unreviewed, by)? == self.unreviewed_sha256)
    }

    pub(super) fn flag(&self, number: usize) -> Option<&LocalFlag> {
        self.flags.iter().find(|flag| flag.number == number)
    }
}

pub(super) fn digest(
    sessions: &[BundledSession],
    by: &BundleAuthor,
) -> Result<String, serde_json::Error> {
    let text = wire::json::canonical_json(&(sessions, by))?;
    Ok(hex::encode(sha2::Sha256::digest(text)))
}
