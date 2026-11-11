use std::collections::BTreeMap;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;

use super::super::key::LocalKey;
use super::super::key::MacFormat;
use super::places::Place;
use super::slots::BaseValue;
use super::slots::FieldAt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalReview {
    pub at: DateTime<Utc>,
    pub unreviewed_sha256: String,
    pub flags: Vec<LocalFlag>,
    pub fields: Vec<BaseField>,
    #[serde(default = "v1", skip_serializing_if = "is_v1")]
    pub mac_format: MacFormat,
    pub seal: String,
}

const fn v1() -> MacFormat {
    MacFormat::V1
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde calls skip_serializing_if with a reference to the field"
)]
fn is_v1(format: &MacFormat) -> bool {
    *format == MacFormat::V1
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

    fn unsealed(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::json::canonical_json(&Self {
            seal: String::new(),
            ..self.clone()
        })
    }

    fn mac(&self, key: &LocalKey) -> Result<String, serde_json::Error> {
        Ok(key.hex(&[b"local review", &self.unsealed()?]))
    }

    #[must_use]
    pub fn holds_its_seal(&self, key: &LocalKey) -> bool {
        self.unsealed()
            .is_ok_and(|unsealed| key.verifies(&[b"local review", &unsealed], &self.seal))
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
