mod facts;
mod key;

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use thiserror::Error;

use crate::BoxError;
use crate::owner::Owner;

pub use self::facts::FACT_NEW_FOR;
pub use self::facts::FactReview;
pub use self::facts::Quote;
pub use self::facts::fact_reviews;
pub use self::key::FilingTarget;
pub use self::key::ReviewKey;
pub use self::key::UnknownReviewKey;

pub const SNOOZE: TimeDelta = TimeDelta::hours(24);

#[derive(Debug, Error)]
pub enum ReviewError {
    #[error("review store is unavailable")]
    Unavailable(#[source] BoxError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Decided { at: DateTime<Utc> },
    Snoozed { until: DateTime<Utc> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum MarkState {
    Decided,
    Snoozed,
}

impl Mark {
    #[must_use]
    pub const fn of(state: MarkState, at: DateTime<Utc>) -> Self {
        match state {
            MarkState::Decided => Self::Decided { at },
            MarkState::Snoozed => Self::Snoozed { until: at },
        }
    }

    #[must_use]
    pub const fn parts(self) -> (MarkState, DateTime<Utc>) {
        match self {
            Self::Decided { at } => (MarkState::Decided, at),
            Self::Snoozed { until } => (MarkState::Snoozed, until),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewMark {
    pub key: ReviewKey,
    pub mark: Mark,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks(BTreeMap<ReviewKey, Mark>);

impl Marks {
    #[must_use]
    pub fn of(marks: Vec<ReviewMark>) -> Self {
        Self(
            marks
                .into_iter()
                .map(|found| (found.key, found.mark))
                .collect(),
        )
    }

    #[must_use]
    pub fn decided(&self, key: &ReviewKey) -> bool {
        matches!(self.0.get(key), Some(Mark::Decided { .. }))
    }

    #[must_use]
    pub fn open(&self, key: &ReviewKey, now: DateTime<Utc>) -> bool {
        match self.0.get(key) {
            None => true,
            Some(Mark::Decided { .. }) => false,
            Some(Mark::Snoozed { until }) => *until <= now,
        }
    }
}

#[async_trait]
pub trait ReviewStore: Send + Sync {
    async fn marks(&self, owner: &Owner) -> Result<Vec<ReviewMark>, ReviewError>;

    async fn mark(&self, owner: &Owner, mark: &ReviewMark) -> Result<(), ReviewError>;
}
