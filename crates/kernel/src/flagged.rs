use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use regex::Regex;
use thiserror::Error;
use values::JsonObject;

use crate::BoxError;

pub const FLAG_AFTER_FAILURES: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FlaggedId(i64);

impl FlaggedId {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }
}

impl fmt::Display for FlaggedId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for FlaggedId {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

impl From<FlaggedId> for i64 {
    fn from(value: FlaggedId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlaggedJob {
    pub id: FlaggedId,
    pub task: String,
    pub key: Option<String>,
    pub subject: Option<String>,
    pub payload: JsonObject,
    pub error: String,
    pub failures: u32,
    pub first_failed_at: Option<DateTime<Utc>>,
    pub last_failed_at: DateTime<Utc>,
    pub flagged_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlaggedGroup {
    pub task: String,
    pub cause: String,
    pub count: u64,
    pub last_failed_at: DateTime<Utc>,
    pub sample: FlaggedJob,
}

#[derive(Debug, Error)]
pub enum FlaggedError {
    #[error("flagged job {0} is not open")]
    Missing(FlaggedId),
    #[error("flagged job {id} is unreadable: {reason}")]
    Invalid { id: FlaggedId, reason: String },
    #[error("flagged jobs are unavailable")]
    Unavailable(#[source] BoxError),
}

#[async_trait]
pub trait FlaggedJobs: Send + Sync {
    async fn flagged(&self) -> Result<Vec<FlaggedJob>, FlaggedError>;

    async fn retry(&self, id: FlaggedId) -> Result<(), FlaggedError>;

    async fn dismiss(&self, id: FlaggedId, at: DateTime<Utc>) -> Result<(), FlaggedError>;
}

#[expect(
    clippy::expect_used,
    reason = "the cause pattern is a constant; the flagged job tests match it"
)]
static VARYING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|\d+")
        .expect("the cause pattern is a constant")
});

#[must_use]
pub fn cause(error: &str) -> String {
    let first = error.lines().next().unwrap_or_default().trim();
    VARYING
        .replace_all(first, |found: &regex::Captures<'_>| {
            if found[0].contains('-') { "…" } else { "N" }.to_owned()
        })
        .into_owned()
}

#[must_use]
pub fn group(jobs: Vec<FlaggedJob>) -> Vec<FlaggedGroup> {
    let mut groups: Vec<FlaggedGroup> = Vec::new();
    for job in jobs {
        let cause = cause(&job.error);
        match groups
            .iter_mut()
            .find(|group| group.task == job.task && group.cause == cause)
        {
            Some(group) => {
                group.count += 1;
                if job.last_failed_at > group.last_failed_at {
                    group.last_failed_at = job.last_failed_at;
                    group.sample = job;
                }
            }
            None => groups.push(FlaggedGroup {
                task: job.task.clone(),
                cause,
                count: 1,
                last_failed_at: job.last_failed_at,
                sample: job,
            }),
        }
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.last_failed_at));
    groups
}
