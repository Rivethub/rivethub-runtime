use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel::queue::WorkerRole;
use kernel::tidy::{ModelLabel, TidyError, WorkerReport, WorkerReportStore};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use workers::{OutageCause, Reporter};

struct Written(mpsc::UnboundedSender<Vec<WorkerReport>>);

#[async_trait]
impl WorkerReportStore for Written {
    async fn report(&self, reports: &[WorkerReport]) -> Result<(), TidyError> {
        self.0
            .send(reports.to_vec())
            .map_err(|error| TidyError::Unavailable(Box::new(error)))
    }

    async fn newest_first(&self, _fresh: Duration) -> Result<Vec<WorkerReport>, TidyError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_worker_reports_its_model_and_then_its_outage() -> Result<(), Box<dyn std::error::Error>>
{
    let (written, mut reports) = mpsc::unbounded_channel();
    let (cause, outage) = watch::channel(None);
    let reporter = Reporter {
        store: Arc::new(Written(written)),
        node: "datahub".to_owned(),
        roles: vec![(
            WorkerRole::Categorizer,
            ModelLabel {
                provider: Some("http://gpu.example:8041/v1".to_owned()),
                model: Some("deepseek".to_owned()),
            },
        )],
        index_key: Some("0a1b".to_owned()),
        outage,
    };
    let cancel = CancellationToken::new();
    let running = tokio::spawn(reporter.run(cancel.clone()));
    let expected = |outage: Option<&str>| {
        vec![WorkerReport {
            node: "datahub".to_owned(),
            role: WorkerRole::Categorizer,
            provider: Some("http://gpu.example:8041/v1".to_owned()),
            model: Some("deepseek".to_owned()),
            outage: outage.map(str::to_owned),
            index_key: Some("0a1b".to_owned()),
        }]
    };
    assert_eq!(reports.recv().await, Some(expected(None)));
    cause.send(Some(OutageCause::Unreachable))?;
    assert_eq!(reports.recv().await, Some(expected(Some("unreachable"))));
    cancel.cancel();
    running.await?;
    Ok(())
}

struct Stuck;

#[async_trait]
impl WorkerReportStore for Stuck {
    async fn report(&self, _reports: &[WorkerReport]) -> Result<(), TidyError> {
        std::future::pending().await
    }

    async fn newest_first(&self, _fresh: Duration) -> Result<Vec<WorkerReport>, TidyError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_report_write_that_never_returns_does_not_hold_up_shutdown()
-> Result<(), Box<dyn std::error::Error>> {
    let (_cause, outage) = watch::channel(None);
    let reporter = Reporter {
        store: Arc::new(Stuck),
        node: "datahub".to_owned(),
        roles: vec![(WorkerRole::Summarizer, ModelLabel::default())],
        index_key: None,
        outage,
    };
    let cancel = CancellationToken::new();
    let running = tokio::spawn(reporter.run(cancel.clone()));
    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), running).await??;
    Ok(())
}
