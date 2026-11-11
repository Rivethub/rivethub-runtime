use std::sync::Arc;

use kernel::memory::MemoryStoreError;
use kernel::memory::SessionDigest;
use kernel::memory::share::Choice;
use kernel::memory::share::SessionChoices;
use kernel::memory::share::ShareFailure;
use kernel::owner::Owner;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;

#[derive(Debug, Clone)]
pub enum SessionStore {
    Postgres(Arc<PostgresMemory>),
    Sqlite(SqliteStore),
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
    ) -> Result<Vec<(BundledSession, Choice)>, ShareFailure> {
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
            let chosen = choices.as_slice().iter().find(|(id, _)| {
                shareable.contains(id)
                    && session
                        .origin
                        .conversation_id
                        .eq_ignore_ascii_case(&id.to_string())
            });
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
            return Err(ShareFailure::UnknownSessions(missing.join(", ")));
        }
        Ok(kept
            .into_iter()
            .map(|(_, session, choice)| (session, choice))
            .collect())
    }
}
