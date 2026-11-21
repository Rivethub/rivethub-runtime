use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::StatusCode;
use gateway::{HubEmbedder, WorkerReporting};
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
    on("datahub", role, provider, model, outage)
}

fn on(
    node: &str,
    role: WorkerRole,
    provider: &str,
    model: &str,
    outage: Option<&str>,
) -> WorkerReport {
    WorkerReport {
        node: node.to_owned(),
        role,
        provider: Some(provider.to_owned()),
        model: Some(model.to_owned()),
        outage: outage.map(str::to_owned),
        index_key: Some(format!("key-of-{model}")),
    }
}

fn reporting(reports: Vec<WorkerReport>, embedder: Option<&str>) -> WorkerReporting {
    WorkerReporting {
        store: Arc::new(Reported(reports)),
        hub: HubEmbedder {
            label: embedder.map(|model| ModelLabel {
                provider: Some(EMBED.to_owned()),
                model: Some(model.to_owned()),
            }),
            index_key: embedder.map(|model| format!("key-of-{model}")),
        },
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
        categorizer.detail.contains("unreachable on datahub")
            && categorizer.detail.contains("1 queued"),
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
async fn nodes_that_run_different_models_are_named_and_any_mismatch_fails() -> TestResult {
    let (_, listed) = listed(reporting(
        vec![
            on(
                "desk",
                WorkerRole::Embedder,
                EMBED,
                "qwen3-embedding-0.6b",
                None,
            ),
            on(
                "datahub",
                WorkerRole::Embedder,
                EMBED,
                "nemotron-embed-8b",
                None,
            ),
            on("desk", WorkerRole::Summarizer, CHAT, "flash", None),
            on(
                "datahub",
                WorkerRole::Summarizer,
                CHAT,
                "deepseek",
                Some("unreachable"),
            ),
        ],
        Some("qwen3-embedding-0.6b"),
    ))
    .await?;
    let summarizer = listed.first().ok_or("summarizer row")?;
    assert_eq!(summarizer.reported_by.as_deref(), Some("datahub, desk"));
    assert_eq!(summarizer.health, ModelHealth::Failing);
    assert_eq!(summarizer.outage.as_deref(), Some("unreachable"));
    assert!(
        summarizer.detail.contains("unreachable on datahub")
            && summarizer
                .detail
                .contains("nodes run different models: flash at")
            && summarizer.detail.contains("deepseek at"),
        "{}",
        summarizer.detail
    );
    let embedder = listed.last().ok_or("embedder row")?;
    assert_eq!(embedder.model.as_deref(), Some("qwen3-embedding-0.6b"));
    assert_eq!(embedder.health, ModelHealth::Failing);
    assert!(
        embedder
            .detail
            .contains("the embedder on datahub indexes with nemotron-embed-8b"),
        "{}",
        embedder.detail
    );
    Ok(())
}

#[tokio::test]
async fn an_embedder_with_the_same_index_key_under_another_label_is_healthy() -> TestResult {
    let mut aliased = report(
        WorkerRole::Embedder,
        "http://embed.example:9402/v1",
        "qwen3-embedding-0.6b",
        None,
    );
    aliased.index_key = Some("key-of-nemotron-embed-8b".to_owned());
    let (_, listed) = listed(reporting(vec![aliased], Some("nemotron-embed-8b"))).await?;
    let embedder = listed.last().ok_or("embedder row")?;
    assert_eq!(embedder.health, ModelHealth::Healthy, "{}", embedder.detail);
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
