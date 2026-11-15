use chrono::DateTime;
use chrono::Utc;
use values::SessionId;

use super::TopicId;
use super::TopicName;
use crate::owner::Owner;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    strum::Display,
    strum::EnumString,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum PlacementSource {
    You,
    Categorizer,
}

impl PlacementSource {
    #[must_use]
    pub fn keeps(self, incoming: Self) -> bool {
        self == Self::You && incoming == Self::Categorizer
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lineage {
    pub parent: SessionId,
    pub task: uuid::Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub session: SessionId,
    pub topic: TopicId,
    pub by: PlacementSource,
    pub at: DateTime<Utc>,
    pub owner: Option<Owner>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementOutcome {
    Placed(Placement),
    Kept(Placement),
}

impl Placement {
    #[must_use]
    pub fn admit(self, existing: Option<Self>) -> PlacementOutcome {
        match existing {
            Some(current) if current.by.keeps(self.by) => PlacementOutcome::Kept(current),
            Some(_) | None => PlacementOutcome::Placed(self),
        }
    }
}

impl PlacementOutcome {
    #[must_use]
    pub fn into_placement(self) -> Placement {
        match self {
            Self::Placed(placement) | Self::Kept(placement) => placement,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMark {
    pub session: SessionId,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedSession {
    pub placement: Placement,
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub session: SessionId,
    pub name: TopicName,
    pub topic: Option<TopicId>,
    pub parent: Option<TopicId>,
}
