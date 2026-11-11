use chrono::Duration;
use chrono::TimeDelta;
use serde::Deserialize;
use serde::Serialize;

use crate::error::WorkerError;

pub(super) const PIPELINE_VERSION: i32 = 6;
pub(super) const MAX_LEAF_ROUNDS: u32 = 10;
pub(super) const LEAF_BATCH: u32 = 10;
pub(super) const BRANCH_BATCH: u32 = 8;
pub(super) const ROOT_BATCH: u32 = 5;
pub(super) const MIN_LEAVES_FOR_BRANCH: u32 = 5;
pub(super) const MIN_BRANCHES_FOR_ROOT: u32 = 3;
pub(super) const IDLE_MINUTES: u32 = 15;
pub(super) const STALE_MINUTES: u32 = 5_760;
pub(super) const STALE_MIN_BATCH: u32 = 2;
const LEAF_MAX_TOKENS: u32 = 16_384;
const BRANCH_MAX_TOKENS: u32 = 24_576;
const ROOT_MAX_TOKENS: u32 = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryTrigger {
    Threshold,
    SessionIdle,
    SessionStale,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySettings {
    pub model: String,
    pub leaf_batch: u32,
    pub branch_batch: u32,
    pub root_batch: u32,
    pub min_leaves_for_branch: u32,
    pub min_branches_for_root: u32,
    pub idle: TimeDelta,
    pub stale: TimeDelta,
    pub stale_min_batch: u32,
    pub max_leaf_rounds: u32,
    pub pipeline_version: i32,
    pub leaf_max_tokens: u32,
    pub branch_max_tokens: u32,
    pub root_max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryParts {
    pub model: String,
    pub leaf_batch: u32,
    pub branch_batch: u32,
    pub root_batch: u32,
    pub min_leaves_for_branch: u32,
    pub min_branches_for_root: u32,
    pub idle_minutes: u32,
    pub stale_minutes: u32,
    pub stale_min_batch: u32,
    pub max_leaf_rounds: u32,
    pub pipeline_version: i32,
}

impl SummarySettings {
    pub fn standard(model: impl Into<String>) -> Result<Self, WorkerError> {
        Self::from_parts(SummaryParts {
            model: model.into(),
            leaf_batch: LEAF_BATCH,
            branch_batch: BRANCH_BATCH,
            root_batch: ROOT_BATCH,
            min_leaves_for_branch: MIN_LEAVES_FOR_BRANCH,
            min_branches_for_root: MIN_BRANCHES_FOR_ROOT,
            idle_minutes: IDLE_MINUTES,
            stale_minutes: STALE_MINUTES,
            stale_min_batch: STALE_MIN_BATCH,
            max_leaf_rounds: MAX_LEAF_ROUNDS,
            pipeline_version: PIPELINE_VERSION,
        })
    }

    pub fn from_parts(parts: SummaryParts) -> Result<Self, WorkerError> {
        require_positive("leaf batch", parts.leaf_batch)?;
        require_positive("branch batch", parts.branch_batch)?;
        require_positive("root batch", parts.root_batch)?;
        require_positive("min leaves for a branch", parts.min_leaves_for_branch)?;
        require_positive("min branches for a root", parts.min_branches_for_root)?;
        require_positive("stale min batch", parts.stale_min_batch)?;
        require_positive("max leaf rounds", parts.max_leaf_rounds)?;
        if parts.pipeline_version <= 0 {
            return Err(invalid("pipeline version must be at least 1"));
        }
        if parts.model.is_empty() {
            return Err(invalid("summary model is empty"));
        }
        Ok(Self {
            model: parts.model,
            leaf_batch: parts.leaf_batch,
            branch_batch: parts.branch_batch,
            root_batch: parts.root_batch,
            min_leaves_for_branch: parts.min_leaves_for_branch,
            min_branches_for_root: parts.min_branches_for_root,
            idle: minutes(parts.idle_minutes)?,
            stale: minutes(parts.stale_minutes)?,
            stale_min_batch: parts.stale_min_batch,
            max_leaf_rounds: parts.max_leaf_rounds,
            pipeline_version: parts.pipeline_version,
            leaf_max_tokens: LEAF_MAX_TOKENS,
            branch_max_tokens: BRANCH_MAX_TOKENS,
            root_max_tokens: ROOT_MAX_TOKENS,
        })
    }

    pub fn with_max_tokens(
        mut self,
        leaf: u32,
        branch: u32,
        root: u32,
    ) -> Result<Self, WorkerError> {
        require_positive("leaf max tokens", leaf)?;
        require_positive("branch max tokens", branch)?;
        require_positive("root max tokens", root)?;
        self.leaf_max_tokens = leaf;
        self.branch_max_tokens = branch;
        self.root_max_tokens = root;
        Ok(self)
    }

    #[must_use]
    pub const fn idle_floor(&self) -> u32 {
        let half = self.leaf_batch / 2;
        if half == 0 { self.leaf_batch } else { half }
    }
}

fn require_positive(name: &str, value: u32) -> Result<(), WorkerError> {
    if value == 0 {
        Err(invalid(format!("{name} must be at least 1")))
    } else {
        Ok(())
    }
}

fn minutes(value: u32) -> Result<TimeDelta, WorkerError> {
    match Duration::try_minutes(i64::from(value)) {
        Some(duration) => Ok(duration),
        None => Err(invalid("a minute window does not fit")),
    }
}

fn invalid(message: impl Into<String>) -> WorkerError {
    WorkerError::SummarySettings {
        message: message.into(),
    }
}
