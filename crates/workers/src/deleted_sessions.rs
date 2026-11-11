use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use kernel::memory::DeletedSession;
use kernel::memory::SessionDeletion;
use kernel::memory::TOMBSTONE_PURGE_BATCH;
use kernel::memory::TRASH_PURGE_BATCH;
use tokio_util::sync::CancellationToken;

use crate::WorkerError;

pub(crate) async fn handle_purge(
    deletions: &dyn SessionDeletion,
    trash_days: u32,
    now: DateTime<Utc>,
    cancel: &CancellationToken,
) -> Result<DeletedSession, WorkerError> {
    let cutoff = now - TimeDelta::days(i64::from(trash_days));
    let mut purged = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = deletions.purge_tombstoned(TOMBSTONE_PURGE_BATCH) => result?,
    };
    if purged != DeletedSession::default() {
        tracing::info!(
            conversations = purged.conversations,
            messages = purged.messages,
            summaries = purged.summaries,
            "purged memory rows written for deleted sessions"
        );
    }
    let emptied = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = deletions.purge_trashed(cutoff, TRASH_PURGE_BATCH) => result?,
    };
    if emptied != DeletedSession::default() {
        tracing::info!(
            conversations = emptied.conversations,
            messages = emptied.messages,
            summaries = emptied.summaries,
            trash_days,
            "purged sessions that stayed in the trash past retention"
        );
    }
    purged += emptied;
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use testkit::FakeSessionDeletion;
    use testkit::topics::at;
    use tokio_util::sync::CancellationToken;
    use wire::session_id::SessionId;

    use super::*;

    #[tokio::test]
    async fn only_sessions_trashed_past_retention_are_purged()
    -> Result<(), Box<dyn std::error::Error>> {
        let deletions = FakeSessionDeletion::default();
        let old = SessionId::new(wire::harness::HarnessId::ClaudeCode, "old");
        let recent = SessionId::new(wire::harness::HarnessId::ClaudeCode, "recent");
        let now = at(0) + TimeDelta::days(40);
        deletions
            .trash_session(old.as_str(), None, now - TimeDelta::days(31))
            .await?;
        deletions
            .trash_session(recent.as_str(), None, now - TimeDelta::days(29))
            .await?;
        handle_purge(&deletions, 30, now, &CancellationToken::new()).await?;
        assert!(!deletions.is_trashed(&old));
        assert!(deletions.is_trashed(&recent));
        assert!(deletions.deleted_by(&old).is_some());
        Ok(())
    }
}
