use super::SummaryStoreError;
use super::summary_backend::{SummaryIndexes, SummaryPermit};
use super::summary_reference::PreparedSummaryTree;
use async_trait::async_trait;
use jiff::SignedDuration;
use jiff::Timestamp;
use serde::Serialize;
use uuid::Uuid;

pub const GIANT_TOKENS: i64 = 1_000_000;
pub const DEFAULT_RATE: i64 = 200_000;
pub const INVENTORY_PAGE_SIZE: i64 = 200;
pub const LEASE: SignedDuration = SignedDuration::from_mins(20);

#[derive(Debug, Clone, Copy)]
pub enum ConversionControl {
    Start {
        rate: Option<i64>,
        include_giants: bool,
    },
    Pause,
    Resume {
        include_giants: bool,
    },
    Retry {
        conversation: Uuid,
    },
    Release {
        conversation: Uuid,
    },
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionProgress {
    pub enabled: bool,
    pub include_giants: bool,
    pub inventory_after: Option<Uuid>,
    pub inventory_complete: bool,
    pub discovered: i64,
    pub pending: i64,
    pub waiting_quiet: i64,
    pub reserved: i64,
    pub complete: i64,
    pub failed: i64,
    pub giant_deferred: i64,
    pub rate: i64,
    pub charged_tokens: i64,
    pub provider_input_tokens: i64,
    pub provider_output_tokens: i64,
    pub current: Option<Uuid>,
    pub recent_failures: Vec<(Uuid, String)>,
}

impl ConversionProgress {
    #[doc(hidden)]
    pub fn count(&mut self, state: &str, count: i64) {
        self.discovered += count;
        match state {
            "pending" => self.pending += count,
            "waiting_quiet" => self.waiting_quiet += count,
            "reserved" => self.reserved += count,
            "complete" => self.complete += count,
            "failed" => self.failed += count,
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConversionLease {
    pub conversation: Uuid,
    pub revision: i64,
    pub claimed_at: Timestamp,
}

#[derive(Debug)]
pub enum ConversionOutcome {
    Yield,
    Complete,
    Fail(String),
}

#[derive(Debug, Clone, Copy)]
pub struct ConversionCharge {
    pub estimate: i64,
    pub capacity: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct ConversionUsage {
    pub estimate: i64,
    pub input: i64,
    pub output: i64,
}

#[async_trait]
pub trait SummaryConversion: Send + Sync {
    async fn control(
        &self,
        action: ConversionControl,
    ) -> Result<ConversionProgress, SummaryStoreError>;
    async fn status(&self) -> Result<ConversionProgress, SummaryStoreError>;

    #[doc(hidden)]
    async fn claim(
        &self,
        inactivity: SignedDuration,
    ) -> Result<Option<ConversionLease>, SummaryStoreError>;

    #[doc(hidden)]
    async fn admit(
        &self,
        lease: ConversionLease,
        charge: Option<ConversionCharge>,
    ) -> Result<bool, SummaryStoreError>;

    #[doc(hidden)]
    async fn freeze(
        &self,
        lease: ConversionLease,
        root: Option<Uuid>,
        manifest: serde_json::Value,
    ) -> Result<Option<Uuid>, SummaryStoreError>;

    #[doc(hidden)]
    async fn try_model_slot(
        &self,
        limit: u32,
    ) -> Result<Option<Box<dyn SummaryPermit>>, SummaryStoreError>;

    #[doc(hidden)]
    async fn report(&self, usage: ConversionUsage) -> Result<(), SummaryStoreError>;

    #[doc(hidden)]
    async fn finish(
        &self,
        lease: ConversionLease,
        outcome: ConversionOutcome,
    ) -> Result<(), SummaryStoreError>;

    #[doc(hidden)]
    async fn publish(
        &self,
        lease: ConversionLease,
        tree: PreparedSummaryTree,
        indexes: SummaryIndexes,
    ) -> Result<bool, SummaryStoreError>;
}

#[doc(hidden)]
#[derive(Debug)]
pub struct ConversionBucket {
    pub rate: i64,
    pub tokens: f64,
    pub at: Timestamp,
}

#[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
impl ConversionBucket {
    pub fn debit(&mut self, charge: ConversionCharge, now: Timestamp) -> bool {
        let elapsed = (now - self.at).as_millis().max(0);
        let capacity = self.rate.max(charge.capacity).max(charge.estimate) as f64;
        let tokens = (self.tokens + elapsed as f64 * self.rate as f64 / 60_000.0).min(capacity);
        if now < self.at {
            return false;
        }
        if tokens < charge.estimate as f64 {
            self.at = now;
            self.defer(charge.estimate as f64 - tokens);
            self.tokens = charge.estimate as f64;
            return false;
        }
        self.tokens = tokens - charge.estimate as f64;
        self.at = now;
        true
    }

    pub fn reconcile(&mut self, usage: ConversionUsage) {
        self.tokens -= usage.input.saturating_sub(usage.estimate) as f64;
        if self.tokens < 0.0 {
            self.defer(-self.tokens);
            self.tokens = 0.0;
        }
    }

    fn defer(&mut self, deficit: f64) {
        let delay = (deficit * 60_000.0 / self.rate as f64).ceil() as i64;
        self.at = self
            .at
            .checked_add(SignedDuration::from_millis(delay.min(i64::MAX / 2)))
            .unwrap_or(Timestamp::MAX);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionRehearsal {
    pub plan: &'static str,
    pub sampled_conversations: usize,
    pub tables: std::collections::BTreeMap<&'static str, ReferenceCounts>,
}

#[derive(Debug, Default, Serialize)]
pub struct ReferenceCounts {
    pub remapped: i64,
    pub deleted: i64,
}

pub const REHEARSAL_LIMIT: usize = 20;

impl Default for ConversionRehearsal {
    fn default() -> Self {
        Self {
            plan: "single session record; actual parts and sections can change convergence",
            sampled_conversations: 0,
            tables: std::collections::BTreeMap::new(),
        }
    }
}

impl ConversionRehearsal {
    #[doc(hidden)]
    pub fn add(&mut self, name: &'static str, total: i64, remapped: i64) {
        let counts = self.tables.entry(name).or_default();
        counts.remapped += remapped;
        counts.deleted += total - remapped;
    }
}

#[cfg(test)]
mod tests {
    use super::{ConversionBucket, ConversionCharge};
    use jiff::{SignedDuration, Timestamp};

    #[test]
    fn conversion_bucket_refills_in_whole_milliseconds_and_caps_large_intervals() {
        let start = Timestamp::UNIX_EPOCH;
        let mut bucket = ConversionBucket {
            rate: 60_000,
            tokens: 2.0,
            at: start,
        };
        let charge = ConversionCharge {
            estimate: 1,
            capacity: 60_000,
        };

        assert!(bucket.debit(charge, start));
        assert_eq!(bucket.tokens.to_bits(), 1.0_f64.to_bits());
        let submillisecond = start + SignedDuration::from_micros(999);
        assert!(bucket.debit(charge, submillisecond));
        assert_eq!(bucket.tokens.to_bits(), 0.0_f64.to_bits());
        assert!(!bucket.debit(charge, start));
        assert!(!bucket.debit(charge, submillisecond));
        let delayed = submillisecond + SignedDuration::from_millis(1);
        assert_eq!(bucket.at, delayed);
        assert!(bucket.debit(charge, delayed));
        assert!(bucket.debit(charge, Timestamp::MAX));
        assert_eq!(bucket.tokens.to_bits(), 59_999.0_f64.to_bits());
    }
}
