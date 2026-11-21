use std::sync::Arc;

use kernel::queue::WorkerRole;
use kernel::tidy::ModelLabel;
use kernel::tidy::REPORT_INTERVAL;
use kernel::tidy::WorkerReport;
use kernel::tidy::WorkerReportStore;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::OutageCause;
use crate::Runner;

impl<Q> Runner<Q> {
    #[must_use]
    pub fn outage(&self) -> watch::Receiver<Option<OutageCause>> {
        self.outage.cause()
    }
}

#[expect(missing_debug_implementations)]
pub struct Reporter {
    pub store: Arc<dyn WorkerReportStore>,
    pub node: String,
    pub roles: Vec<(WorkerRole, ModelLabel)>,
    pub outage: watch::Receiver<Option<OutageCause>>,
}

impl Reporter {
    fn reports(&mut self) -> Vec<WorkerReport> {
        let outage = (*self.outage.borrow_and_update()).map(|cause| cause.to_string());
        self.roles
            .iter()
            .map(|(role, label)| WorkerReport {
                node: self.node.clone(),
                role: *role,
                provider: label.provider.clone(),
                model: label.model.clone(),
                outage: outage.clone(),
            })
            .collect()
    }

    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            let reports = self.reports();
            if let Err(error) = self.store.report(&reports).await {
                tracing::warn!("worker report was not written: {error}");
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                changed = self.outage.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(REPORT_INTERVAL) => {}
            }
        }
    }
}
