use std::collections::BTreeSet;
use std::sync::Arc;

use kernel::flagged::FLAG_AFTER_FAILURES;
use kernel::tidy::{ModelLabel, RoleActivity, TaskActivity, WorkerReport, WorkerReportStore};
use wire::gateway::memory_models::{ModelHealth, ModelJob, ModelRole};

use crate::models::worker_role;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubEmbedder {
    pub label: Option<ModelLabel>,
    pub index_key: Option<String>,
}

#[derive(Clone)]
#[expect(missing_debug_implementations)]
pub struct WorkerReporting {
    pub store: Arc<dyn WorkerReportStore>,
    pub hub: HubEmbedder,
}

const REPORTED: [ModelRole; 5] = [
    ModelRole::Summarizer,
    ModelRole::Categorizer,
    ModelRole::WikiWriter,
    ModelRole::EntityBuilder,
    ModelRole::Embedder,
];

pub(crate) fn reported(
    newest_first: &[WorkerReport],
    hub: &HubEmbedder,
    tasks: &[TaskActivity],
) -> Vec<ModelJob> {
    REPORTED
        .iter()
        .map(|&role| {
            let worker = worker_role(role);
            let activity = worker
                .map(|worker| RoleActivity::of(worker, tasks))
                .unwrap_or_default();
            let mine: Vec<&WorkerReport> = newest_first
                .iter()
                .filter(|report| Some(report.role) == worker)
                .collect();
            match mine.split_first() {
                Some((newest, _)) => {
                    let mismatch = (role == ModelRole::Embedder)
                        .then(|| {
                            mine.iter()
                                .find_map(|report| embedder_mismatch(hub, report))
                        })
                        .flatten();
                    running(role, newest, &mine, mismatch, activity)
                }
                None => not_running(role, activity),
            }
        })
        .collect()
}

fn not_running(role: ModelRole, activity: RoleActivity) -> ModelJob {
    let waiting = if activity.queued > 0 {
        format!("; {} queued", activity.queued)
    } else {
        String::new()
    };
    ModelJob {
        role,
        provider: None,
        model: None,
        inherits_default: false,
        health: ModelHealth::NotRunning,
        detail: format!("no worker has reported this role in the last two minutes{waiting}"),
        queued: activity.queued,
        outage: None,
        reported_by: None,
    }
}

fn running(
    role: ModelRole,
    newest: &WorkerReport,
    all: &[&WorkerReport],
    mismatch: Option<String>,
    activity: RoleActivity,
) -> ModelJob {
    let outage = all
        .iter()
        .find_map(|report| report.outage.as_deref().map(|cause| (cause, &report.node)));
    let stopped = outage.is_some() || mismatch.is_some();
    let health = if stopped || activity.failing >= u64::from(FLAG_AFTER_FAILURES) {
        ModelHealth::Failing
    } else if activity.failing > 0 {
        ModelHealth::Slow
    } else {
        ModelHealth::Healthy
    };
    let mut parts = Vec::new();
    if let Some((cause, node)) = outage {
        parts.push(format!(
            "the model provider is {cause} on {node}; the worker stopped claiming"
        ));
    }
    parts.extend(mismatch);
    parts.extend(differing(all));
    if activity.failing > 0 {
        parts.push(format!("{} failing", activity.failing));
    }
    if activity.queued > 0 {
        parts.push(format!("{} queued", activity.queued));
    } else if parts.is_empty() {
        parts.push("idle".to_owned());
    }
    let nodes: BTreeSet<&str> = all.iter().map(|report| report.node.as_str()).collect();
    ModelJob {
        role,
        provider: newest.provider.clone(),
        model: newest.model.clone(),
        inherits_default: false,
        health,
        detail: parts.join("; "),
        queued: activity.queued,
        outage: outage.map(|(cause, _)| cause.to_owned()),
        reported_by: Some(nodes.into_iter().collect::<Vec<_>>().join(", ")),
    }
}

fn named(provider: Option<&str>, model: Option<&str>) -> String {
    format!(
        "{} at {}",
        model.unwrap_or("an unnamed model"),
        provider.unwrap_or("an unnamed endpoint")
    )
}

fn differing(all: &[&WorkerReport]) -> Option<String> {
    let distinct: BTreeSet<(&Option<String>, &Option<String>)> = all
        .iter()
        .map(|report| (&report.provider, &report.model))
        .collect();
    (distinct.len() > 1).then(|| {
        let each: Vec<String> = all
            .iter()
            .map(|report| {
                format!(
                    "{} on {}",
                    named(report.provider.as_deref(), report.model.as_deref()),
                    report.node
                )
            })
            .collect();
        format!("nodes run different models: {}", each.join(", "))
    })
}

#[must_use]
pub fn embedder_mismatch(hub: &HubEmbedder, report: &WorkerReport) -> Option<String> {
    let (Some(mine), Some(theirs)) = (hub.index_key.as_deref(), report.index_key.as_deref()) else {
        return None;
    };
    if mine == theirs {
        return None;
    }
    let indexed = named(report.provider.as_deref(), report.model.as_deref());
    let searched = hub
        .label
        .as_ref()
        .map(|label| named(label.provider.as_deref(), label.model.as_deref()))
        .filter(|searched| *searched != indexed);
    let node = &report.node;
    Some(match searched {
        Some(searched) => format!(
            "this hub embeds search text with {searched}, but the embedder on {node} indexes \
             with {indexed}; search by meaning misses summaries until both name the same \
             endpoint and model"
        ),
        None => format!(
            "this hub and the embedder on {node} ({indexed}) index summaries under different \
             settings, such as the API prefix or the vector width; search by meaning misses \
             summaries until they match"
        ),
    })
}
