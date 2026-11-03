use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryOwner;
use kernel::memory::TreeWindow;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::gateway::memory::MemorySummaryChildren;
use wire::gateway::memory::MemorySummaryCounts;
use wire::gateway::memory::MemorySummaryDagResponse;
use wire::gateway::memory::MemorySummaryNode;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Query;
use crate::extract::SinceParam;

const EXPORT_MEDIA_TYPE: &str = "application/gzip";

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ExportParams {
    since: Option<SinceParam>,
}

const LATEST_MAX: usize = 50;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct DagParams {
    outline: Option<bool>,
    tree: Option<bool>,
    offset: Option<usize>,
    limit: Option<usize>,
    latest: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TreeParams {
    offset: Option<usize>,
    limit: Option<usize>,
}

fn window(offset: Option<usize>, limit: Option<usize>) -> TreeWindow {
    TreeWindow {
        offset: offset.unwrap_or(TreeWindow::ALL.offset),
        limit: limit.unwrap_or(TreeWindow::ALL.limit),
    }
}

async fn read_dag(
    gateway: &Gateway,
    owner: &SummaryOwner,
    params: &DagParams,
) -> Result<SummaryDag, ApiError> {
    let store = gateway.store();
    Ok(if let Some(latest) = params.latest {
        store
            .summary_latest(owner, latest.clamp(1, LATEST_MAX))
            .await?
    } else if params.outline == Some(true) {
        store.summary_outline(owner).await?
    } else {
        store.summary_dag(owner).await?
    })
}

fn dag_body(session_id: String, dag: SummaryDag) -> MemorySummaryDagResponse {
    MemorySummaryDagResponse {
        session_id,
        summaries: dag
            .summaries
            .into_iter()
            .map(crate::memory_body::memory_summary_node_body)
            .collect(),
        messages: crate::memory_body::memory_message_tally_body(dag.messages),
        counts: None,
    }
}

fn nodes(tree: Vec<kernel::memory::MemorySummaryNode>) -> Vec<MemorySummaryNode> {
    tree.into_iter()
        .map(crate::memory_body::memory_summary_node_body)
        .collect()
}

pub(crate) async fn summaries(
    State(gateway): State<Arc<Gateway>>,
    Path(session): Path<String>,
    Query(params): Query<DagParams>,
) -> Result<Json<MemorySummaryDagResponse>, ApiError> {
    let owner = SummaryOwner::Session(session.clone());
    if params.tree == Some(true) && params.latest.is_none() {
        let tree = gateway
            .store()
            .summary_tree(&owner, None, window(params.offset, params.limit))
            .await?
            .unwrap_or_default();
        return Ok(Json(MemorySummaryDagResponse {
            session_id: session,
            summaries: nodes(tree.summaries),
            messages: crate::memory_body::memory_message_tally_body(tree.messages),
            counts: Some(MemorySummaryCounts {
                leaves: tree.counts.leaves,
                branches: tree.counts.branches,
                roots: tree.counts.roots,
                embedded: tree.counts.embedded,
                top: tree.counts.top,
            }),
        }));
    }
    let dag = read_dag(&gateway, &owner, &params).await?;
    Ok(Json(dag_body(session, dag)))
}

pub(crate) async fn children(
    State(gateway): State<Arc<Gateway>>,
    Path((session, summary)): Path<(String, String)>,
    Query(params): Query<TreeParams>,
) -> Result<Json<MemorySummaryChildren>, ApiError> {
    Uuid::parse_str(&summary)
        .map_err(|_| ApiError::BadRequest(format!("{summary} is not a summary id")))?;
    let tree = gateway
        .store()
        .summary_tree(
            &SummaryOwner::Session(session),
            Some(&summary),
            window(params.offset, params.limit),
        )
        .await?
        .ok_or_else(|| ApiError::Missing(format!("unknown summary {summary}")))?;
    Ok(Json(MemorySummaryChildren {
        summaries: nodes(tree.summaries),
        total: tree.total,
    }))
}

pub(crate) async fn summary(
    State(gateway): State<Arc<Gateway>>,
    Path((session, summary)): Path<(String, String)>,
) -> Result<Json<MemorySummaryNode>, ApiError> {
    Uuid::parse_str(&summary)
        .map_err(|_| ApiError::BadRequest(format!("{summary} is not a summary id")))?;
    gateway
        .store()
        .summary_node(&SummaryOwner::Session(session), &summary)
        .await?
        .map(|node| Json(crate::memory_body::memory_summary_node_body(node)))
        .ok_or_else(|| ApiError::Missing(format!("unknown summary {summary}")))
}

pub(crate) async fn conversation_summaries(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
    Query(params): Query<DagParams>,
) -> Result<Json<MemorySummaryDagResponse>, ApiError> {
    let conversation = Uuid::parse_str(&segment)
        .map_err(|_| ApiError::BadRequest(format!("{segment} is not a conversation id")))?;
    let dag = read_dag(&gateway, &SummaryOwner::Conversation(conversation), &params).await?;
    let session_id = dag
        .session_key
        .clone()
        .ok_or_else(|| ApiError::Missing(format!("unknown conversation {segment}")))?;
    Ok(Json(dag_body(session_id, dag)))
}

pub(crate) async fn export(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<ExportParams>,
) -> Result<Response, ApiError> {
    let exporter = gateway
        .memory_export()
        .ok_or_else(|| ApiError::Unavailable("memory export is not configured".to_owned()))?;
    let cancellation = CancellationToken::new();
    let abandoned = cancellation.clone().drop_guard();
    let dump = exporter
        .export(params.since.map(SinceParam::get), cancellation)
        .await?;
    drop(abandoned);
    let disposition = format!(
        "attachment; filename=\"memory-{}.sql.gz\"",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    Ok((
        [
            (header::CONTENT_TYPE, EXPORT_MEDIA_TYPE.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        dump,
    )
        .into_response())
}
