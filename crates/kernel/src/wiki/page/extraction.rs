use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{WikiHistoryEntry, WikiSlug};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum WikiAction {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WikiPatch {
    pub action: WikiAction,
    pub slug: WikiSlug,
    pub title: String,
    pub aliases: Vec<WikiSlug>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub summary: String,
    pub article: Option<String>,
    pub related: Option<Vec<WikiSlug>>,
    pub history_entry: Option<WikiHistoryEntry>,
    #[serde(rename = "type")]
    pub entity_type: Option<String>,
    pub facts: Option<Vec<PatchFact>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchFact {
    pub key: String,
    pub value: String,
    pub evidence: String,
}

impl WikiPatch {
    #[must_use]
    pub fn note(&self, day: NaiveDate) -> WikiHistoryEntry {
        let article = self.article.as_deref().map_or("", str::trim);
        let article = match article.strip_prefix("# ") {
            Some(rest) => rest
                .split_once('\n')
                .map_or("", |(_, body)| body.trim_start()),
            None => article,
        };
        WikiHistoryEntry {
            date: self.history_entry.as_ref().map_or(day, |entry| entry.date),
            title: self.title.trim().to_owned(),
            body: [self.summary.trim(), article]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}
