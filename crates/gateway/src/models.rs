use kernel::flagged::FLAG_AFTER_FAILURES;
use kernel::model::{ProviderHealth, ProviderRecovery, ProviderStatus};
use kernel::queue::WorkerRole;
use kernel::tidy::{RoleActivity, TaskActivity};
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
        ModelRole::Embedder => Some(WorkerRole::Embedder),
    }
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
