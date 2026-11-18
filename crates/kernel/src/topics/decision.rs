use jiff::Timestamp;
use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use uuid::Uuid;
use values::SessionId;

use super::Topic;
use super::TopicError;
use super::TopicId;

pub const DECISIONS_HEADING: &str = "Decisions";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecisionId(Uuid);

impl DecisionId {
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

impl fmt::Display for DecisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for DecisionId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionText(String);

impl DecisionText {
    pub fn new(text: &str) -> Result<Self, TopicError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            Err(TopicError::EmptyDecision)
        } else {
            Ok(Self(trimmed.to_owned()))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum DecisionStatus {
    Active,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: DecisionId,
    pub topic: TopicId,
    pub text: DecisionText,
    pub status: DecisionStatus,
    pub superseded_by: Option<DecisionId>,
    pub by: String,
    pub session: Option<SessionId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Decision {
    #[must_use]
    pub fn new(
        topic: TopicId,
        text: DecisionText,
        by: &str,
        session: Option<SessionId>,
        now: Timestamp,
    ) -> Self {
        Self {
            id: DecisionId::random(),
            topic,
            text,
            status: DecisionStatus::Active,
            superseded_by: None,
            by: by.to_owned(),
            session,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn edit(&self, text: DecisionText, now: Timestamp) -> Result<Self, TopicError> {
        self.active()?;
        Ok(Self {
            text,
            updated_at: now,
            ..self.clone()
        })
    }

    pub fn supersede(
        &self,
        replacement: Option<&Self>,
        now: Timestamp,
    ) -> Result<Self, TopicError> {
        self.active()?;
        if replacement.is_some_and(|next| next.topic != self.topic || next.id == self.id) {
            return Err(TopicError::ForeignReplacement);
        }
        Ok(Self {
            status: DecisionStatus::Superseded,
            superseded_by: replacement.map(|next| next.id),
            updated_at: now,
            ..self.clone()
        })
    }

    fn active(&self) -> Result<(), TopicError> {
        match self.status {
            DecisionStatus::Active => Ok(()),
            DecisionStatus::Superseded => Err(TopicError::DecisionSuperseded { id: self.id }),
        }
    }

    fn line(&self) -> String {
        let when = self.created_at.strftime("%Y-%m-%d");
        match self.status {
            DecisionStatus::Active => format!("- {} ({}, {when})", self.text.as_str(), self.by),
            DecisionStatus::Superseded => format!(
                "- ~~{}~~ superseded ({}, {when})",
                self.text.as_str(),
                self.by
            ),
        }
    }
}

#[async_trait]
pub trait DecisionStore: Send + Sync {
    async fn decisions(&self, topic: TopicId) -> Result<Vec<Decision>, TopicError>;

    async fn decision(&self, id: DecisionId) -> Result<Option<Decision>, TopicError>;

    async fn keep_decisions(&self, decisions: &[Decision]) -> Result<(), TopicError>;
}

pub async fn topic_decision(
    store: &dyn DecisionStore,
    topic: TopicId,
    id: DecisionId,
) -> Result<Decision, TopicError> {
    store
        .decision(id)
        .await?
        .filter(|decision| decision.topic == topic)
        .ok_or_else(|| TopicError::UnknownDecision {
            key: id.to_string(),
        })
}

#[must_use]
pub fn render_decisions(own: &[Decision], subtopics: &[(&Topic, Vec<Decision>)]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    if !own.is_empty() {
        blocks.push(lines(own));
    }
    for (topic, decisions) in subtopics.iter().filter(|(_, list)| !list.is_empty()) {
        blocks.push(format!(
            "### {}\n\n{}",
            topic.name.as_str(),
            lines(decisions)
        ));
    }
    blocks.join("\n\n")
}

fn lines(decisions: &[Decision]) -> String {
    decisions
        .iter()
        .map(Decision::line)
        .collect::<Vec<_>>()
        .join("\n")
}
