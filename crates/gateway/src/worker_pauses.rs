use kernel::queue::pause::PausableRole;
use kernel::queue::pause::RolePause;
use kernel::tidy::RoleActivity;
use kernel::tidy::TaskActivity;
use strum::IntoEnumIterator;
use wire::gateway::worker_pauses::PausableRole as PausableRoleBody;
use wire::gateway::worker_pauses::WorkerPause;
use wire::gateway::worker_pauses::WorkerPauses;

#[must_use]
pub fn listed(paused: &[RolePause], tasks: &[TaskActivity]) -> WorkerPauses {
    WorkerPauses {
        roles: PausableRole::iter()
            .map(|role| status(role, paused.iter().find(|pause| pause.role == role), tasks))
            .collect(),
    }
}

#[must_use]
pub fn status(
    role: PausableRole,
    pause: Option<&RolePause>,
    tasks: &[TaskActivity],
) -> WorkerPause {
    WorkerPause {
        role: match role {
            PausableRole::Categorizer => PausableRoleBody::Categorizer,
            PausableRole::WikiBuilder => PausableRoleBody::WikiBuilder,
            PausableRole::EntityBuilder => PausableRoleBody::EntityBuilder,
        },
        paused: pause.is_some(),
        paused_at: pause.map(|pause| pause.paused_at),
        paused_by: pause.and_then(|pause| pause.paused_by.clone()),
        queued: RoleActivity::of(role.worker_role(), tasks).queued,
    }
}
