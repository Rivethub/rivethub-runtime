use std::time::Duration;

use async_trait::async_trait;

use super::TidyError;
use crate::queue::WorkerRole;

pub const REPORT_INTERVAL: Duration = Duration::from_secs(30);
pub const REPORT_FRESH: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelLabel {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerReport {
    pub node: String,
    pub role: WorkerRole,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub outage: Option<String>,
    pub index_key: Option<String>,
}

#[async_trait]
pub trait WorkerReportStore: Send + Sync {
    async fn report(&self, reports: &[WorkerReport]) -> Result<(), TidyError>;

    async fn newest_first(&self, fresh: Duration) -> Result<Vec<WorkerReport>, TidyError>;
}
