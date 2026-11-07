mod derive;
mod digest;
mod today;

use ::values::SessionId;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;

use crate::BoxError;
use crate::facts::Claim;
use crate::topics::PlacementSource;
use crate::topics::TopicId;
use crate::topics::TopicKind;

pub use self::derive::changes;
pub use self::derive::fact_change;
pub use self::derive::fact_text;
pub use self::digest::DIGEST_MAX_CHARS;
pub use self::digest::DIGEST_SYSTEM_PROMPT;
pub use self::digest::DailyDigest;
pub use self::digest::DigestRequest;
pub use self::digest::DigestStore;
pub use self::digest::clean_digest;
pub use self::digest::digest_prompt;
pub use self::today::ActivityDay;
pub use self::today::RECENTLY_UPDATED;
pub use self::today::activity;
pub use self::today::fallback_summary;
pub use self::today::headline;
pub use self::today::learned;
pub use self::today::recently_updated;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum ChangeKind {
    Fact,
    Decision,
    New,
    Filed,
    Conflict,
    Tag,
    Merge,
}

impl ChangeKind {
    #[must_use]
    pub const fn learned(self) -> bool {
        matches!(self, Self::Fact | Self::Decision | Self::New)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeActor {
    You,
    WikiWriter,
    Categorizer,
}

impl From<PlacementSource> for ChangeActor {
    fn from(source: PlacementSource) -> Self {
        match source {
            PlacementSource::You => Self::You,
            PlacementSource::Categorizer => Self::Categorizer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subject {
    Topic { id: TopicId, title: String },
    Entity { id: TopicId, title: String },
    Session(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicRef {
    pub id: TopicId,
    pub title: String,
    pub kind: TopicKind,
}

impl From<TopicRef> for Subject {
    fn from(topic: TopicRef) -> Self {
        let TopicRef { id, title, kind } = topic;
        match kind {
            TopicKind::Entity => Self::Entity { id, title },
            TopicKind::Work | TopicKind::Type => Self::Topic { id, title },
        }
    }
}

impl Subject {
    #[must_use]
    pub fn key(&self) -> SubjectKey {
        match self {
            Self::Topic { id, .. } | Self::Entity { id, .. } => SubjectKey::Topic(*id),
            Self::Session(session) => SubjectKey::Session(session.clone()),
        }
    }

    #[must_use]
    pub fn link(&self) -> Option<String> {
        match self {
            Self::Topic { id, title } => Some(format!("[{}](/topics/{id})", link_label(title))),
            Self::Entity { id, title } => Some(format!("[{}](/entities/{id})", link_label(title))),
            Self::Session(_) => None,
        }
    }
}

fn link_label(title: &str) -> String {
    title.replace(['[', ']'], "")
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SubjectKey {
    Topic(TopicId),
    Session(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub subject: Subject,
    pub text: String,
    pub at: DateTime<Utc>,
    pub actor: ChangeActor,
    pub source: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeWindow {
    pub from: Option<DateTime<Utc>>,
    pub to: DateTime<Utc>,
    pub subject: Option<SubjectKey>,
}

impl ChangeWindow {
    #[must_use]
    pub const fn between(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        Self {
            from: Some(from),
            to,
            subject: None,
        }
    }

    #[must_use]
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.from.is_none_or(|from| at >= from) && at < self.to
    }

    #[must_use]
    pub fn concerns(&self, change: &Change) -> bool {
        self.subject.as_ref().is_none_or(|subject| {
            *subject == change.subject.key()
                || matches!(subject, SubjectKey::Session(session) if change.source.as_ref() == Some(session))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeRecord {
    Claim {
        entity: TopicId,
        title: String,
        claim: Claim,
    },
    Decision {
        topic: TopicRef,
        text: String,
        session: Option<SessionId>,
        at: DateTime<Utc>,
    },
    Created {
        topic: TopicRef,
        at: DateTime<Utc>,
    },
    Placed {
        topic: TopicRef,
        session: SessionId,
        by: PlacementSource,
        at: DateTime<Utc>,
    },
    Tagged {
        session: SessionId,
        key: String,
        value: String,
        at: DateTime<Utc>,
    },
    Merged {
        into: TopicRef,
        from: String,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Error)]
pub enum ChangeError {
    #[error("change feed is unavailable")]
    Unavailable(#[source] BoxError),
    #[error("daily digest payload: {0}")]
    Payload(#[source] BoxError),
}

#[async_trait]
pub trait ChangeFeed: Send + Sync {
    async fn records(&self, window: &ChangeWindow) -> Result<Vec<ChangeRecord>, ChangeError>;

    async fn changes(&self, window: &ChangeWindow) -> Result<Vec<Change>, ChangeError> {
        Ok(changes(self.records(window).await?, window))
    }
}
