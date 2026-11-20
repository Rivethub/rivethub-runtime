use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use parking_lot::Mutex;
use sqlx::PgExecutor;

use crate::error::Error;

const RECHECK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backfill {
    MessageCounts,
    ChunkSaid,
}

impl Backfill {
    const fn name(self) -> &'static str {
        match self {
            Self::MessageCounts => "message-counts",
            Self::ChunkSaid => "chunk-said",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Backfills {
    settled: AtomicBool,
    seen: Mutex<Option<(Instant, Vec<String>)>>,
}

impl Backfills {
    pub(crate) async fn done<'e>(
        &self,
        executor: impl PgExecutor<'e>,
        backfill: Backfill,
    ) -> Result<bool, Error> {
        if self.settled.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let recent = self
            .seen
            .lock()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < RECHECK)
            .map(|(_, pending)| pending.clone());
        let pending = if let Some(pending) = recent {
            pending
        } else {
            let pending: Vec<String> = sqlx::query_scalar("SELECT name FROM rh_backfills")
                .fetch_all(executor)
                .await
                .map_err(|source| Error::operation("backfill state", source))?;
            *self.seen.lock() = Some((Instant::now(), pending.clone()));
            pending
        };
        if pending.is_empty() {
            self.settled.store(true, Ordering::Relaxed);
        }
        Ok(!pending.iter().any(|name| name == backfill.name()))
    }
}
