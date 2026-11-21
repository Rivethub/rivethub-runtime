use kernel::flagged::FLAG_AFTER_FAILURES;
use kernel::model::{ProviderHealth, ProviderRecovery, ProviderStatus};
use kernel::queue::WorkerRole;
use std::sync::Arc;

use kernel::tidy::{ModelLabel, RoleActivity, TaskActivity, WorkerReport, WorkerReportStore};
use wire::gateway::memory_models::{ModelHealth, ModelJob, ModelRole};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAssignment {
    pub role: ModelRole,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub inherits_default: bool,
}

const fn worker_role(role: ModelRole) -> Option<WorkerRole> {
    match role {
        ModelRole::Default => None,
        ModelRole::Summarizer => Some(WorkerRole::Summarizer),
        ModelRole::Categorizer => Some(WorkerRole::Categorizer),
        ModelRole::WikiWriter => Some(WorkerRole::WikiBuilder),
        ModelRole::EntityBuilder => Some(WorkerRole::EntityBuilder),
        ModelRole::Embedder => Some(WorkerRole::Embedder),
    }
}

const UNCONFIGURED: [(ModelRole, &str); 6] = [
    (ModelRole::Default, "no memory job has a model"),
    (
        ModelRole::Summarizer,
        "summaries and session titles are off",
    ),
    (
        ModelRole::Categorizer,
        "entities, tags and topic filing are off",
    ),
    (ModelRole::WikiWriter, "wiki pages are off"),
    (
        ModelRole::EntityBuilder,
        "entity pages and merge decisions are off",
    ),
    (
        ModelRole::Embedder,
        "embeddings are off and search ranks by keywords only",
    ),
];

pub(crate) fn unconfigured(tasks: &[TaskActivity]) -> Vec<ModelJob> {
    UNCONFIGURED
        .iter()
        .map(|&(role, off)| {
            let activity = worker_role(role)
                .map(|worker| RoleActivity::of(worker, tasks))
                .unwrap_or_default();
            let parked = if activity.queued > 0 {
                format!("; {} queued jobs wait for a model", activity.queued)
            } else {
                String::new()
            };
            ModelJob {
                role,
                provider: None,
                model: None,
                inherits_default: false,
                health: ModelHealth::Unconfigured,
                detail: format!("no model is configured: {off}{parked}"),
                queued: activity.queued,
                outage: None,
                reported_by: None,
            }
        })
        .collect()
}

pub(crate) fn report(
    assignments: &[ModelAssignment],
    providers: &[ProviderHealth],
    tasks: &[TaskActivity],
) -> Vec<ModelJob> {
    assignments
        .iter()
        .map(|assignment| job(assignment, assignments, providers, tasks))
        .collect()
}

struct ProviderTrouble {
    recovery: ProviderRecovery,
    cause: String,
}

fn provider_trouble(
    assignment: &ModelAssignment,
    providers: &[ProviderHealth],
) -> Option<ProviderTrouble> {
    let id = assignment.provider.as_ref()?;
    let health = providers.iter().find(|health| &health.id == id)?;
    match &*health.status.borrow() {
        ProviderStatus::Ready => None,
        ProviderStatus::Degraded(degradation) => Some(ProviderTrouble {
            recovery: degradation.recovery,
            cause: degradation.cause.to_string(),
        }),
    }
}

fn job(
    assignment: &ModelAssignment,
    all: &[ModelAssignment],
    providers: &[ProviderHealth],
    tasks: &[TaskActivity],
) -> ModelJob {
    let activity = worker_role(assignment.role)
        .map(|role| RoleActivity::of(role, tasks))
        .unwrap_or_default();
    let trouble = provider_trouble(assignment, providers);
    let permanent = trouble
        .as_ref()
        .is_some_and(|trouble| trouble.recovery == ProviderRecovery::Permanent);
    let health = if permanent || activity.failing >= u64::from(FLAG_AFTER_FAILURES) {
        ModelHealth::Failing
    } else if trouble.is_some() || activity.failing > 0 {
        ModelHealth::Slow
    } else {
        ModelHealth::Healthy
    };
    ModelJob {
        role: assignment.role,
        provider: assignment.provider.clone(),
        model: assignment.model.clone(),
        inherits_default: assignment.inherits_default,
        health,
        detail: detail(assignment, all, trouble.as_ref(), activity),
        queued: activity.queued,
        outage: None,
        reported_by: None,
    }
}

fn detail(
    assignment: &ModelAssignment,
    all: &[ModelAssignment],
    trouble: Option<&ProviderTrouble>,
    activity: RoleActivity,
) -> String {
    let mut parts = Vec::new();
    if let Some(trouble) = trouble {
        parts.push(format!("provider {}: {}", trouble.recovery, trouble.cause));
    }
    if activity.failing > 0 {
        parts.push(format!("{} failing", activity.failing));
    }
    if assignment.role == ModelRole::Default {
        let users: Vec<String> = all
            .iter()
            .filter(|other| other.role != ModelRole::Default && other.inherits_default)
            .map(|other| other.role.to_string())
            .collect();
        parts.push(if users.is_empty() {
            "no job inherits it".to_owned()
        } else {
            format!("inherited by {}", users.join(", "))
        });
    } else if activity.queued > 0 {
        parts.push(format!("{} queued", activity.queued));
    } else if parts.is_empty() {
        parts.push("idle".to_owned());
    }
    parts.join("; ")
}

#[derive(Clone)]
#[expect(missing_debug_implementations)]
pub struct WorkerReporting {
    pub store: Arc<dyn WorkerReportStore>,
    pub hub_embedder: Option<ModelLabel>,
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
    hub_embedder: Option<&ModelLabel>,
    tasks: &[TaskActivity],
) -> Vec<ModelJob> {
    REPORTED
        .iter()
        .map(|&role| {
            let worker = worker_role(role);
            let activity = worker
                .map(|worker| RoleActivity::of(worker, tasks))
                .unwrap_or_default();
            let mut mine = newest_first
                .iter()
                .filter(|report| Some(report.role) == worker);
            match mine.next() {
                Some(newest) => {
                    let mut nodes: Vec<&str> = std::iter::once(newest)
                        .chain(mine)
                        .map(|report| report.node.as_str())
                        .collect();
                    nodes.sort_unstable();
                    nodes.dedup();
                    let mismatch = hub_embedder
                        .filter(|_| role == ModelRole::Embedder)
                        .and_then(|own| embedder_mismatch(own, newest));
                    running(role, newest, &nodes.join(", "), mismatch, activity)
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
    report: &WorkerReport,
    nodes: &str,
    mismatch: Option<String>,
    activity: RoleActivity,
) -> ModelJob {
    let stopped = report.outage.is_some() || mismatch.is_some();
    let health = if stopped || activity.failing >= u64::from(FLAG_AFTER_FAILURES) {
        ModelHealth::Failing
    } else if activity.failing > 0 {
        ModelHealth::Slow
    } else {
        ModelHealth::Healthy
    };
    let mut parts = Vec::new();
    if let Some(outage) = &report.outage {
        parts.push(format!(
            "the model provider is {outage}; the worker stopped claiming"
        ));
    }
    parts.extend(mismatch);
    if activity.failing > 0 {
        parts.push(format!("{} failing", activity.failing));
    }
    if activity.queued > 0 {
        parts.push(format!("{} queued", activity.queued));
    } else if parts.is_empty() {
        parts.push("idle".to_owned());
    }
    ModelJob {
        role,
        provider: report.provider.clone(),
        model: report.model.clone(),
        inherits_default: false,
        health,
        detail: parts.join("; "),
        queued: activity.queued,
        outage: report.outage.clone(),
        reported_by: Some(nodes.to_owned()),
    }
}

#[must_use]
pub fn embedder_mismatch(own: &ModelLabel, report: &WorkerReport) -> Option<String> {
    let named = |provider: &Option<String>, model: &Option<String>| {
        format!(
            "{} at {}",
            model.as_deref().unwrap_or("an unnamed model"),
            provider.as_deref().unwrap_or("an unnamed endpoint")
        )
    };
    (own.provider != report.provider || own.model != report.model).then(|| {
        format!(
            "this hub embeds search text with {}, but the embedder on {} indexes with {}; \
             search by meaning misses summaries until both name the same endpoint and model",
            named(&own.provider, &own.model),
            report.node,
            named(&report.provider, &report.model)
        )
    })
}
