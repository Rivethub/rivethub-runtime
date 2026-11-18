use jiff::Timestamp;
use kernel::memory::DeletedSession;
use kernel::memory::SessionDeletion;
use kernel::memory::TOMBSTONE_PURGE_BATCH;
use kernel::memory::TRASH_PURGE_BATCH;
use tokio_util::sync::CancellationToken;

use crate::WorkerError;

pub(crate) async fn handle_purge(
    deletions: &dyn SessionDeletion,
    trash_days: u32,
    now: Timestamp,
    cancel: &CancellationToken,
) -> Result<DeletedSession, WorkerError> {
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
    let cutoff = now.checked_sub(wire::time::elapsed_days(i64::from(trash_days)));
    let emptied = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = async {
            match cutoff {
                Ok(cutoff) => deletions.purge_trashed(cutoff, TRASH_PURGE_BATCH).await,
                Err(_out_of_range) => Ok(DeletedSession::default()),
            }
        } => result?,
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

    use testkit::FakeSessionDeletion;
    use testkit::topics::at;
    use tokio_util::sync::CancellationToken;
    use wire::session_id::SessionId;

    use super::*;

    #[tokio::test]
    async fn cancellation_after_tombstone_purge_wins_over_an_out_of_range_cutoff() {
        let cancel = CancellationToken::new();
        let deletions = FakeSessionDeletion::default().cancel_after_tombstones(cancel.clone());
        let result = handle_purge(&deletions, 5_000_000, at(0), &cancel).await;
        assert!(matches!(result, Err(WorkerError::Cancelled)));
        assert_eq!(deletions.tombstone_purges(), 1);
    }

    #[tokio::test]
    async fn large_trash_retention_skips_trash_but_still_purges_tombstones()
    -> Result<(), Box<dyn std::error::Error>> {
        let deletions = FakeSessionDeletion::default();
        let session = SessionId::new(wire::harness::HarnessId::ClaudeCode, "keep-trashed");
        deletions
            .trash_session(session.as_str(), None, Timestamp::MIN)
            .await?;
        handle_purge(&deletions, 5_000_000, at(0), &CancellationToken::new()).await?;
        assert!(deletions.is_trashed(&session));
        assert_eq!(deletions.tombstone_purges(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn only_sessions_trashed_past_retention_are_purged()
    -> Result<(), Box<dyn std::error::Error>> {
        let deletions = FakeSessionDeletion::default();
        let old = SessionId::new(wire::harness::HarnessId::ClaudeCode, "old");
        let recent = SessionId::new(wire::harness::HarnessId::ClaudeCode, "recent");
        let now = at(0) + wire::time::elapsed_days(40);
        deletions
            .trash_session(old.as_str(), None, now - wire::time::elapsed_days(31))
            .await?;
        deletions
            .trash_session(recent.as_str(), None, now - wire::time::elapsed_days(29))
            .await?;
        handle_purge(&deletions, 30, now, &CancellationToken::new()).await?;
        assert!(!deletions.is_trashed(&old));
        assert!(deletions.is_trashed(&recent));
        assert!(deletions.deleted_by(&old).is_some());
        Ok(())
    }
}
