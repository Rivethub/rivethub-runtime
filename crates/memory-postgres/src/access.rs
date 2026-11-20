use std::collections::BTreeSet;
use std::sync::Arc;

use kernel::tenancy::Scope;
use parking_lot::Mutex;
use sqlx::PgPool;
use uuid::Uuid;

use crate::Error;

const PENDING_LIMIT: usize = 10_000;

#[derive(Debug, Default)]
pub(crate) struct AccessLog {
    state: Mutex<Pending>,
    idle: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct Pending {
    messages: BTreeSet<Uuid>,
    summaries: BTreeSet<Uuid>,
    dropped: u64,
    writing: bool,
    failure: Option<Arc<Error>>,
}

impl AccessLog {
    pub(crate) fn record(
        self: &Arc<Self>,
        pool: &PgPool,
        messages: Vec<Uuid>,
        summaries: Vec<Uuid>,
    ) {
        let start = {
            let mut pending = self.state.lock();
            for id in messages {
                pending.add_message(id);
            }
            for id in summaries {
                pending.add_summary(id);
            }
            let idle = !pending.writing && !pending.is_empty();
            if idle {
                pending.writing = true;
            }
            idle
        };
        if start {
            tokio::spawn(Scope::Maintenance.run(Arc::clone(self).drain(pool.clone())));
        }
    }

    pub(crate) async fn settled(&self) -> Result<(), Arc<Error>> {
        loop {
            let idle = self.idle.notified();
            {
                let pending = self.state.lock();
                if !pending.writing && pending.is_empty() {
                    return match &pending.failure {
                        Some(error) => Err(Arc::clone(error)),
                        None => Ok(()),
                    };
                }
            }
            idle.await;
        }
    }

    async fn drain(self: Arc<Self>, pool: PgPool) {
        loop {
            let (messages, summaries, dropped) = {
                let mut pending = self.state.lock();
                if pending.is_empty() {
                    pending.writing = false;
                    drop(pending);
                    self.idle.notify_waiters();
                    return;
                }
                (
                    std::mem::take(&mut pending.messages),
                    std::mem::take(&mut pending.summaries),
                    std::mem::take(&mut pending.dropped),
                )
            };
            if dropped > 0 {
                tracing::warn!(
                    "search access counts: {dropped} hit(s) dropped while the writer was behind"
                );
            }
            if let Err(error) = write(&pool, &messages, &summaries).await {
                tracing::warn!("search access counts not recorded: {error}");
                self.state
                    .lock()
                    .failure
                    .get_or_insert_with(|| Arc::new(error));
            }
        }
    }
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.summaries.is_empty() && self.dropped == 0
    }

    fn has_capacity(&self) -> bool {
        self.messages.len() + self.summaries.len() < PENDING_LIMIT
    }

    fn add_message(&mut self, id: Uuid) {
        if self.has_capacity() || self.messages.contains(&id) {
            self.messages.insert(id);
        } else {
            self.dropped += 1;
        }
    }

    fn add_summary(&mut self, id: Uuid) {
        if self.has_capacity() || self.summaries.contains(&id) {
            self.summaries.insert(id);
        } else {
            self.dropped += 1;
        }
    }
}

async fn write(
    pool: &PgPool,
    messages: &BTreeSet<Uuid>,
    summaries: &BTreeSet<Uuid>,
) -> Result<(), Error> {
    if !messages.is_empty() {
        let ids: Vec<Uuid> = messages.iter().copied().collect();
        sqlx::query!(
            "insert into rh_message_access (message_id, access_count, last_accessed_at)
             select m.id, m.access_count + 1, now() from rh_messages m where m.id = any($1)
             on conflict (message_id) do update
                set access_count = rh_message_access.access_count + 1, last_accessed_at = now()",
            &ids
        )
        .execute(pool)
        .await
        .map_err(|source| Error::operation("access bump", source))?;
    }
    if !summaries.is_empty() {
        let ids: Vec<Uuid> = summaries.iter().copied().collect();
        let mut tx = pool
            .begin()
            .await
            .map_err(|source| Error::operation("access begin", source))?;
        let guarded = crate::summary_replace::shared_visible(&mut tx, &ids)
            .await
            .map_err(|error| Error::stored("summary access input", error))?;
        let ids: Vec<_> = guarded.into_iter().map(|input| input.id).collect();
        sqlx::query!(
            "insert into rh_summary_access (summary_id, access_count, last_accessed_at)
             select s.id, s.access_count + 1, now() from rh_summaries s where s.id = any($1)
             on conflict (summary_id) do update
                set access_count = rh_summary_access.access_count + 1, last_accessed_at = now()",
            &ids
        )
        .execute(&mut *tx)
        .await
        .map_err(|source| Error::operation("access bump", source))?;
        tx.commit()
            .await
            .map_err(|source| Error::operation("access commit", source))?;
    }
    Ok(())
}
