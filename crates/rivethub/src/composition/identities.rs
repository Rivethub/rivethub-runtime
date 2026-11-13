use std::sync::Arc;
use std::time::Duration;

use gateway::tenancy::{Tenancy, UserId};
use kernel::tenancy::Scope;
use kernel::tenancy::Space;
use kernel::tenancy::SpaceId;
use kernel::tenancy::SpaceKind;
use kernel::tenancy::TenancyStore;
use tokio_util::sync::CancellationToken;
use wire::gateway::users::UsersRegistry;

use super::ComposeError;

const HOUSEHOLD: &str = "household";
const HOUSEHOLD_NAME: &str = "Household";
const SPACE_SYNC_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct Identity {
    pub(crate) tenancy: Tenancy,
    pub(super) owner: UserId,
}

impl Identity {
    pub(crate) fn primary(&self) -> kernel::owner::Owner {
        self.owner.owner().clone()
    }

    pub(crate) fn owner_scope(&self) -> Scope {
        Scope::Viewer(self.tenancy.viewer(&self.owner))
    }
}

pub(crate) async fn load(
    config: &config::Config,
    env: &config::Environment,
) -> Result<Identity, ComposeError> {
    let path = env.users_file().map(std::path::Path::new).or_else(|| {
        config
            .gateway
            .as_ref()
            .and_then(|gateway| gateway.users_file.as_deref())
    });
    let Some(path) = path else {
        return registry(UsersRegistry {
            unmapped_is_owner: true,
            ..UsersRegistry::default()
        });
    };
    let users: UsersRegistry = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    registry(users)
}

const fn space_kind(kind: wire::gateway::tenancy::SpaceKind) -> SpaceKind {
    match kind {
        wire::gateway::tenancy::SpaceKind::Household => SpaceKind::Household,
        wire::gateway::tenancy::SpaceKind::Team => SpaceKind::Team,
        wire::gateway::tenancy::SpaceKind::Compartment => SpaceKind::Compartment,
    }
}

fn invalid(error: impl std::fmt::Display) -> ComposeError {
    ComposeError::Invalid(format!("users registry: {error}"))
}

fn spaces(users: &UsersRegistry, owner: &UserId) -> Result<Vec<Space>, ComposeError> {
    let Some(configured) = &users.spaces else {
        let mut members = vec![owner.owner().clone()];
        for id in users.users.keys() {
            let user = UserId::new(id.clone())?;
            if !members.contains(user.owner()) {
                members.push(user.owner().clone());
            }
        }
        return Ok(vec![Space {
            id: SpaceId::new(HOUSEHOLD.to_owned()).map_err(invalid)?,
            kind: SpaceKind::Household,
            name: HOUSEHOLD_NAME.to_owned(),
            members,
        }]);
    };
    configured
        .iter()
        .map(|(id, space)| {
            Ok(Space {
                id: SpaceId::new(id.clone()).map_err(invalid)?,
                kind: space_kind(space.kind),
                name: space.name.clone().unwrap_or_else(|| id.clone()),
                members: space
                    .members
                    .iter()
                    .map(|member| UserId::new(member.clone()).map(|user| user.owner().clone()))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect()
}

fn registry(users: UsersRegistry) -> Result<Identity, ComposeError> {
    let owner = UserId::new(users.owner_user_id.clone())?;
    let mut tenancy = Tenancy::local(owner.clone()).with_spaces(spaces(&users, &owner)?);
    for (id, record) in users.users {
        let user = UserId::new(id)?;
        if record.admin == Some(true) {
            tenancy = tenancy.with_admin(user.clone());
        }
        for token in record.tokens.iter().flatten() {
            tenancy = tenancy.with_users_file_token(token, user.clone())?;
        }
    }
    Ok(Identity { tenancy, owner })
}

pub(super) fn keep_spaces(
    tracker: &tokio_util::task::TaskTracker,
    store: Option<Arc<dyn TenancyStore>>,
    identity: &Identity,
    shutdown: &CancellationToken,
) {
    if let Some(store) = store {
        let spaces = identity.tenancy.spaces().to_vec();
        tracker.spawn(Scope::Maintenance.run(sync_spaces(store, spaces, shutdown.child_token())));
    }
}

async fn sync_spaces(store: Arc<dyn TenancyStore>, spaces: Vec<Space>, stop: CancellationToken) {
    loop {
        match store.sync_spaces(&spaces).await {
            Ok(()) => return,
            Err(error) => tracing::warn!("spaces not saved yet, retrying: {error}"),
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(SPACE_SYNC_RETRY) => {}
        }
    }
}
