use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::StatusCode;
use gateway::WorkerReporting;
use kernel::queue::{FixedJob, WorkerRole};
use kernel::tidy::{ModelLabel, TidyError, WorkerReport, WorkerReportStore};
use wire::gateway::memory_models::{ModelHealth, ModelJob, ModelRole};

use super::TestResult;
use super::models::{OWNER_DEVICE, jobs, models, node_built, queue, roles};

const CHAT: &str = "http://gpu.example:8003/v1";
const EMBED: &str = "http://embed.example:9402";

struct Reported(Vec<WorkerReport>);

#[async_trait]
impl WorkerReportStore for Reported {
    async fn report(&self, _reports: &[WorkerReport]) -> Result<(), TidyError> {
        Ok(())
    }

    async fn newest_first(&self, _fresh: Duration) -> Result<Vec<WorkerReport>, TidyError> {
        Ok(self.0.clone())
    }
}

fn report(role: WorkerRole, provider: &str, model: &str, outage: Option<&str>) -> WorkerReport {
    WorkerReport {
        node: "datahub".to_owned(),
        role,
        provider: Some(provider.to_owned()),
        model: Some(model.to_owned()),
        outage: outage.map(str::to_owned),
    }
}

fn reporting(reports: Vec<WorkerReport>, embedder: Option<&str>) -> WorkerReporting {
    WorkerReporting {
        store: Arc::new(Reported(reports)),
        hub_embedder: embedder.map(|model| ModelLabel {
            provider: Some(EMBED.to_owned()),
            model: Some(model.to_owned()),
        }),
    }
}

async fn listed(reporting: WorkerReporting) -> TestResult<(String, Vec<ModelJob>)> {
    let node = node_built(Vec::new(), roles(), Some(reporting))?;
    queue(&node, FixedJob::FileConversation, "f1").await?;
    queue(&node, FixedJob::ExtractEntities, "e1").await?;
    let (status, bytes) = models(&node, OWNER_DEVICE).await?;
    assert_eq!(status, StatusCode::OK);
    Ok((String::from_utf8(bytes.clone())?, jobs(&bytes)?))
}

#[tokio::test]
async fn a_hub_shows_what_its_workers_report_and_not_its_own_configuration() -> TestResult {
    let (text, listed) = listed(reporting(
        vec![
            report(WorkerRole::Summarizer, CHAT, "flash", None),
            report(
                WorkerRole::Categorizer,
                "http://gpu.example:8041/v1",
                "deepseek",
                Some("unreachable"),
            ),
            report(WorkerRole::Embedder, EMBED, "qwen3-embedding-0.6b", None),
        ],
        Some("qwen3-embedding-0.6b"),
    ))
    .await?;
    let summary: Vec<_> = listed
        .iter()
        .map(|job| (job.role, job.model.as_deref(), job.health, job.queued))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                ModelRole::Summarizer,
                Some("flash"),
                ModelHealth::Healthy,
                0
            ),
            (
                ModelRole::Categorizer,
                Some("deepseek"),
                ModelHealth::Failing,
                1
            ),
            (ModelRole::WikiWriter, None, ModelHealth::NotRunning, 0),
            (ModelRole::EntityBuilder, None, ModelHealth::NotRunning, 1),
            (
                ModelRole::Embedder,
                Some("qwen3-embedding-0.6b"),
                ModelHealth::Healthy,
                0
            ),
        ]
    );
    assert!(text.contains("\"health\":\"not-running\""), "{text}");
    assert!(text.contains("\"reportedBy\":\"datahub\""), "{text}");
    assert!(!text.contains("big-model"), "{text}");
    let summarizer = listed.first().ok_or("summarizer row")?;
    assert_eq!(summarizer.provider.as_deref(), Some(CHAT));
    assert!(!summarizer.inherits_default);
    let categorizer = listed.get(1).ok_or("categorizer row")?;
    assert_eq!(categorizer.outage.as_deref(), Some("unreachable"));
    assert!(
        categorizer.detail.contains("stopped claiming") && categorizer.detail.contains("1 queued"),
        "{}",
        categorizer.detail
    );
    Ok(())
}

#[tokio::test]
async fn an_embedder_the_hub_names_differently_is_failing() -> TestResult {
    let (_, listed) = listed(reporting(
        vec![report(
            WorkerRole::Embedder,
            EMBED,
            "qwen3-embedding-0.6b",
            None,
        )],
        Some("nemotron-embed-8b"),
    ))
    .await?;
    let embedder = listed.last().ok_or("embedder row")?;
    assert_eq!(embedder.health, ModelHealth::Failing);
    assert!(
        embedder.detail.contains("nemotron-embed-8b")
            && embedder.detail.contains("qwen3-embedding-0.6b")
            && embedder.detail.contains("datahub"),
        "{}",
        embedder.detail
    );
    Ok(())
}

#[tokio::test]
async fn a_hub_with_no_model_and_no_worker_is_unconfigured() -> TestResult {
    let node = node_built(Vec::new(), Vec::new(), Some(reporting(Vec::new(), None)))?;
    let (status, bytes) = models(&node, OWNER_DEVICE).await?;
    assert_eq!(status, StatusCode::OK);
    let listed = jobs(&bytes)?;
    assert_eq!(listed.len(), 6);
    assert!(
        listed
            .iter()
            .all(|job| job.health == ModelHealth::Unconfigured)
    );
    Ok(())
}
