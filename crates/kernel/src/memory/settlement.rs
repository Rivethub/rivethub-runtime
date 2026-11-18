use jiff::Timestamp;
use serde::Serialize;
use uuid::Uuid;

pub const DEFAULT_INACTIVITY_MINUTES: u32 = 24 * 60;
pub const SETTLEMENT_PAGE_SIZE: u32 = 200;

#[doc(hidden)]
#[must_use]
pub fn session_uuid(session: &str) -> Option<Uuid> {
    match Uuid::parse_str(session) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::trace!(%error, "looking up settlement by session key");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryState {
    pub conversation_id: Uuid,
    pub input_revision: i64,
    pub last_input_at: Timestamp,
    pub unsettled_since: Option<Timestamp>,
    pub settled_at: Option<Timestamp>,
    pub summarized_through: Option<i64>,
    pub failed_revision: Option<i64>,
    pub historical_only: bool,
    pub last_observed_event_at: Option<Timestamp>,
    pub last_observed_end_at: Option<Timestamp>,
    pub last_observed_end_key: Option<String>,
}

impl SummaryState {
    #[must_use]
    pub fn record_capture(
        &mut self,
        now: Timestamp,
        changed: bool,
        imported: bool,
        observation: Option<(Timestamp, Option<&str>)>,
    ) -> bool {
        let mut dirty = changed;
        if changed {
            self.input_revision += 1;
            self.last_input_at = now;
            self.unsettled_since.get_or_insert(now);
            self.settled_at = None;
            self.failed_revision = None;
            self.historical_only &= imported;
        }
        if let Some((at, end_key)) = observation {
            let current = self.last_observed_event_at;
            if let Some(key) = end_key
                && current.is_none_or(|current| at >= current)
                && self.last_observed_end_at.is_none_or(|current| at > current)
                && self.last_observed_end_key.as_deref() != Some(key)
            {
                self.last_observed_end_at = Some(at);
                self.last_observed_end_key = Some(key.to_owned());
                self.settle(now);
                dirty = true;
            }
            if current.is_none_or(|current| at > current) {
                self.last_observed_event_at = Some(at);
                dirty = true;
            }
        }
        dirty
    }

    pub fn settle(&mut self, now: Timestamp) {
        self.settled_at.get_or_insert(now);
        self.unsettled_since = None;
    }

    #[must_use]
    pub fn manual_settle(&mut self, now: Timestamp) -> SettleResult {
        let status = if self.summarized_through == Some(self.input_revision) {
            SettleStatus::AlreadySummarized
        } else {
            let status = if self.settled_at.is_some()
                && !self.historical_only
                && self.failed_revision.is_none()
            {
                SettleStatus::AlreadySettled
            } else {
                SettleStatus::Settled
            };
            self.settle(now);
            self.historical_only = false;
            self.failed_revision = None;
            status
        };
        SettleResult {
            conversation_id: self.conversation_id,
            input_revision: self.input_revision,
            settled_at: self.settled_at,
            status,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SettleStatus {
    Settled,
    AlreadySettled,
    AlreadySummarized,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResult {
    pub conversation_id: Uuid,
    pub input_revision: i64,
    #[serde_as(as = "Option<values::time::Rfc3339>")]
    pub settled_at: Option<Timestamp>,
    pub status: SettleStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementPage {
    pub next: Option<Uuid>,
    pub settled: u64,
}
