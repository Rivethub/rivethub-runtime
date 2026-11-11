use async_trait::async_trait;
use chrono::DateTime;
use chrono::NaiveDate;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use values::JsonObject;

use super::Change;
use super::ChangeError;
use super::ChangeWindow;
use crate::owner::Owner;
use crate::queue::FixedJob;
use crate::queue::Job;
use crate::queue::JobKeyMode;
use crate::queue::JobName;
use crate::queue::MAINTENANCE_PRIORITY;

pub const DIGEST_MAX_CHARS: usize = 900;
const PROMPT_CHANGES: usize = 60;

pub const DIGEST_SYSTEM_PROMPT: &str = "You write the daily journal entry of a personal memory system. \
You are given what its agents recorded on one day: facts about entities, decisions, new topics, sessions filed under \
topics, tags and conflicts. Write one short paragraph of two or three plain sentences saying what the day was about and \
what was learned. Mention the most important topics or entities with the markdown links exactly as given, for example \
[Backups](/topics/0000). Do not invent links, facts or names. No headings, no lists, no preamble.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyDigest {
    pub day: NaiveDate,
    pub owner: Owner,
    pub summary: String,
    pub changes: u32,
    pub written_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DigestRequest {
    pub day: NaiveDate,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub owner: String,
}

impl DigestRequest {
    #[must_use]
    pub fn new(day: NaiveDate, from: DateTime<Utc>, to: DateTime<Utc>, owner: &Owner) -> Self {
        Self {
            day,
            from,
            to,
            owner: owner.as_str().to_owned(),
        }
    }

    #[must_use]
    pub const fn window(&self) -> ChangeWindow {
        ChangeWindow::between(self.from, self.to)
    }

    pub fn owner(&self) -> Result<Owner, ChangeError> {
        Owner::new(self.owner.clone()).map_err(|source| ChangeError::Payload(Box::new(source)))
    }

    pub fn job(&self) -> Result<Job, ChangeError> {
        Ok(Job {
            name: JobName::Fixed(FixedJob::DigestDay),
            payload: JsonObject::from_serialize(self)
                .map_err(|source| ChangeError::Payload(Box::new(source)))?,
            key: Some(format!("digest-day:{}:{}", self.owner, self.day)),
            key_mode: JobKeyMode::Replace,
            priority: MAINTENANCE_PRIORITY,
            run_at: None,
            max_attempts: None,
        })
    }

    pub fn from_payload(payload: &JsonObject) -> Result<Self, ChangeError> {
        payload
            .deserialize()
            .map_err(|source| ChangeError::Payload(Box::new(source)))
    }
}

#[async_trait]
pub trait DigestStore: Send + Sync {
    async fn digest(
        &self,
        day: NaiveDate,
        owner: &Owner,
    ) -> Result<Option<DailyDigest>, ChangeError>;

    async fn keep_digest(&self, digest: &DailyDigest) -> Result<(), ChangeError>;

    async fn request_digest(&self, request: &DigestRequest) -> Result<(), ChangeError>;
}

#[must_use]
pub fn digest_prompt(day: NaiveDate, changes: &[Change]) -> String {
    let mut lines = vec![
        format!("Day: {}", day.format("%A, %B %-d, %Y")),
        "Recorded changes, newest first:".to_owned(),
    ];
    lines.extend(changes.iter().take(PROMPT_CHANGES).map(prompt_line));
    if changes.len() > PROMPT_CHANGES {
        lines.push(format!(
            "({} more not shown)",
            changes.len() - PROMPT_CHANGES
        ));
    }
    lines.join("\n")
}

fn prompt_line(change: &Change) -> String {
    let on = change
        .subject
        .link()
        .unwrap_or_else(|| "a session".to_owned());
    format!("- {} on {on}: {}", change.kind, change.text)
}

#[must_use]
pub fn clean_digest(text: &str) -> Option<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    let mut chars = flat.chars();
    let head: String = chars.by_ref().take(DIGEST_MAX_CHARS).collect();
    if chars.next().is_none() {
        return Some(head);
    }
    let whole_sentences = head.rfind(". ").and_then(|end| head.get(..=end));
    Some(whole_sentences.unwrap_or(&head).trim_end().to_owned())
}
