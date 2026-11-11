use std::sync::Arc;

use kernel::memory::MemoryStoreError;
use kernel::memory::SessionDigest;
use kernel::memory::share::Choice;
use kernel::memory::share::SessionChoices;
use kernel::owner::Owner;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use thiserror::Error;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;

#[derive(Debug, Clone)]
pub enum SessionStore {
    Postgres(Arc<PostgresMemory>),
    Sqlite(SqliteStore),
}

#[derive(Debug, Error)]
pub enum ChosenError {
    #[error(
        "no session this node can share has the id {0}; it is unknown, trashed or not the node owner's, and nothing was written"
    )]
    Unknown(String),
    #[error(transparent)]
    Store(#[from] MemoryStoreError),
}

impl SessionStore {
    pub async fn digests(
        &self,
        owner: &Owner,
        keys: &[String],
    ) -> Result<Vec<SessionDigest>, MemoryStoreError> {
        Ok(match self {
            Self::Postgres(store) => store
                .session_digests(owner, keys)
                .await
                .map_err(MemoryStoreError::from)?,
            Self::Sqlite(store) => store
                .session_digests(owner, keys.to_vec())
                .await
                .map_err(MemoryStoreError::from)?,
        })
    }

    pub async fn chosen(
        &self,
        author: &BundleAuthor,
        owner: &Owner,
        choices: &SessionChoices,
    ) -> Result<Vec<(BundledSession, Choice)>, ChosenError> {
        let ids: Vec<Uuid> = choices.ids().collect();
        let sessions = match self {
            Self::Postgres(store) => store
                .bundled_sessions(author, Some(&ids))
                .await
                .map_err(MemoryStoreError::from)?,
            Self::Sqlite(store) => store
                .bundled_sessions(
                    author.clone(),
                    Some(ids.iter().map(ToString::to_string).collect()),
                )
                .await
                .map_err(MemoryStoreError::from)?,
        };
        let keys: Vec<String> = sessions
            .iter()
            .map(|session| session.batch.session_key.clone())
            .collect();
        let shareable: Vec<Uuid> = self
            .digests(owner, &keys)
            .await?
            .into_iter()
            .map(|digest| digest.conversation_id)
            .collect();
        let mut kept = Vec::with_capacity(ids.len());
        for session in sessions {
            let chosen = Uuid::parse_str(&session.origin.conversation_id)
                .ok()
                .filter(|id| shareable.contains(id))
                .and_then(|id| choices.as_slice().iter().find(|(chosen, _)| *chosen == id));
            if let Some((id, choice)) = chosen {
                kept.push((*id, session.forwarding_stored_origin(), choice.clone()));
            }
        }
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !kept.iter().any(|(kept, _, _)| kept == *id))
            .map(ToString::to_string)
            .collect();
        if !missing.is_empty() {
            return Err(ChosenError::Unknown(missing.join(", ")));
        }
        Ok(kept
            .into_iter()
            .map(|(_, session, choice)| (session, choice))
            .collect())
    }
}
