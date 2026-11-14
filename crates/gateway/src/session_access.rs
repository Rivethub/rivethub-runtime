use std::collections::BTreeMap;

use kernel::tenancy::SessionVisibility;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use wire::gateway::session_catalog::SharedWith;
use wire::session_id::SessionKey;

use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

const OWNER_ONLY: &str = "only the person who owns this session can change it";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Owner,
    Reader,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) access: Access,
    pub(crate) shared_with: SharedWith,
}

impl Seen {
    pub(crate) const fn readable(&self) -> bool {
        !matches!(self.access, Access::Hidden)
    }

    pub(crate) fn change(&self) -> Result<(), ApiError> {
        match self.access {
            Access::Owner => Ok(()),
            Access::Reader => Err(ApiError::Forbidden(OWNER_ONLY.to_owned())),
            Access::Hidden => Err(ApiError::NotFound("not found")),
        }
    }
}

fn shared_with_viewer(viewer: &Viewer, visibility: &Visibility) -> bool {
    match visibility {
        Visibility::Private => false,
        Visibility::Space(space) => viewer.spaces().contains(space),
    }
}

fn decide(
    gateway: &Gateway,
    user: &UserId,
    viewer: &Viewer,
    secured: bool,
    state: Option<&SessionVisibility>,
) -> Access {
    let node_person = gateway.tenancy.local_user() == Some(user);
    let stored = state
        .filter(|_| secured)
        .and_then(|state| state.owner.as_ref().map(|owner| (owner, &state.effective)));
    match stored {
        Some((owner, _)) if owner == user.owner() => Access::Owner,
        Some((_, effective)) if shared_with_viewer(viewer, effective) => Access::Reader,
        None if node_person => Access::Owner,
        Some(_) | None => Access::Hidden,
    }
}

pub(crate) async fn seen(
    gateway: &Gateway,
    user: &UserId,
    keys: &[SessionKey],
) -> Result<BTreeMap<SessionKey, Seen>, ApiError> {
    let viewer = gateway.tenancy.viewer(user);
    let (secured, mut states) = match gateway.tenancy_store.as_ref() {
        Some(store) if !keys.is_empty() => (
            store.row_secured(),
            store
                .session_visibility(&viewer, keys)
                .await
                .map_err(crate::routes::topics::visibility::error)?,
        ),
        _ => (false, BTreeMap::new()),
    };
    Ok(keys
        .iter()
        .map(|key| {
            let state = states.remove(key);
            let access = decide(gateway, user, &viewer, secured, state.as_ref());
            let shared_with = state.map_or(SharedWith::Private, |state| {
                crate::tenancy::shared_with(&state.effective)
            });
            (
                key.clone(),
                Seen {
                    access,
                    shared_with,
                },
            )
        })
        .collect())
}

pub(crate) async fn seen_one(
    gateway: &Gateway,
    user: &UserId,
    key: &SessionKey,
) -> Result<Seen, ApiError> {
    seen(gateway, user, std::slice::from_ref(key))
        .await?
        .remove(key)
        .ok_or(ApiError::NotFound("not found"))
}

pub(crate) async fn change_key(
    gateway: &Gateway,
    user: &UserId,
    key: &SessionKey,
) -> Result<(), ApiError> {
    seen_one(gateway, user, key).await?.change()
}

pub(crate) async fn change_conversation(
    gateway: &Gateway,
    user: &UserId,
    conversation: uuid::Uuid,
) -> Result<(), ApiError> {
    let Some(store) = gateway
        .tenancy_store
        .as_ref()
        .filter(|store| store.row_secured())
    else {
        return Ok(());
    };
    let owner = store
        .conversation_owner(conversation)
        .await
        .map_err(crate::routes::topics::visibility::error)?;
    match owner {
        Some(owner) if &owner != user.owner() => Err(ApiError::Forbidden(OWNER_ONLY.to_owned())),
        Some(_) | None => Ok(()),
    }
}
