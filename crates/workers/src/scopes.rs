use std::sync::Arc;

use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::TenancyStore;
use uuid::Uuid;

use crate::error::WorkerError;

#[expect(
    missing_debug_implementations,
    reason = "the tenancy store is a trait object"
)]
#[derive(Clone)]
pub struct OwnerScopes {
    pub store: Arc<dyn TenancyStore>,
    pub primary: Owner,
}

impl OwnerScopes {
    pub(crate) async fn of_conversation(&self, conversation: Uuid) -> Result<Scope, WorkerError> {
        match self.store.conversation_owner(conversation).await? {
            Some(owner) => Ok(Scope::Viewer(self.store.viewer_of(&owner).await?)),
            None => Ok(Scope::Closed),
        }
    }

    pub(crate) async fn of_owner(&self, owner: &Owner) -> Result<Scope, WorkerError> {
        Ok(Scope::Viewer(self.store.viewer_of(owner).await?))
    }

    pub(crate) async fn of_topic_owner(&self, owner: Option<&Owner>) -> Result<Scope, WorkerError> {
        match owner {
            Some(owner) => self.of_owner(owner).await,
            None => self.primary().await,
        }
    }

    pub(crate) async fn primary(&self) -> Result<Scope, WorkerError> {
        Ok(Scope::Viewer(self.store.viewer_of(&self.primary).await?))
    }
}

pub(crate) async fn conversation_scope(
    scopes: Option<&OwnerScopes>,
    conversation: Uuid,
) -> Result<Option<Scope>, WorkerError> {
    match scopes {
        Some(scopes) => scopes.of_conversation(conversation).await.map(Some),
        None => Ok(None),
    }
}

pub(crate) async fn owner_scope(
    scopes: Option<&OwnerScopes>,
    owner: &Owner,
) -> Result<Option<Scope>, WorkerError> {
    match scopes {
        Some(scopes) => scopes.of_owner(owner).await.map(Some),
        None => Ok(None),
    }
}

pub(crate) async fn within<F: Future>(scope: Option<Scope>, work: F) -> F::Output {
    match scope {
        Some(scope) => scope.run(work).await,
        None => work.await,
    }
}
