use std::time::Duration;

use kernel::wiki::entity_pass::EntityStoreError;
use rusqlite::{Connection, Transaction, params};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use super::{error, query, write};
use crate::SqliteStore;

const TTL_SECONDS: i64 = 60;
const HEARTBEAT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
pub(super) struct Key {
    pub(super) id: Uuid,
    owner: Uuid,
}

impl Key {
    fn new(id: Uuid) -> Self {
        Self {
            id,
            owner: Uuid::new_v4(),
        }
    }

    fn available(self, tx: &Transaction<'_>, now: i64) -> Result<bool, EntityStoreError> {
        tx.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM rh_entity_leases
             WHERE entity_id = ?1 AND expires_at > ?2)",
            params![self.id.to_string(), now],
            |row| row.get(0),
        )
        .map_err(query)
    }

    fn claim(self, tx: &Transaction<'_>, now: i64) -> Result<bool, EntityStoreError> {
        let changed = tx
            .execute(
                "INSERT INTO rh_entity_leases (entity_id, owner, expires_at)
                 VALUES (?1, ?2, ?3 + ?4)
                 ON CONFLICT (entity_id) DO UPDATE
                    SET owner = excluded.owner, expires_at = excluded.expires_at
                 WHERE rh_entity_leases.expires_at <= ?3",
                params![
                    self.id.to_string(),
                    self.owner.to_string(),
                    now,
                    TTL_SECONDS
                ],
            )
            .map_err(query)?;
        Ok(changed == 1)
    }

    pub(super) fn renew(self, tx: &Transaction<'_>) -> Result<bool, EntityStoreError> {
        let changed = tx
            .execute(
                "UPDATE rh_entity_leases SET expires_at = unixepoch() + ?3
                 WHERE entity_id = ?1 AND owner = ?2 AND expires_at > unixepoch()",
                params![self.id.to_string(), self.owner.to_string(), TTL_SECONDS],
            )
            .map_err(query)?;
        Ok(changed == 1)
    }

    pub(super) fn check(self, conn: &Connection) -> Result<(), EntityStoreError> {
        let held: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM rh_entity_leases
                 WHERE entity_id = ?1 AND owner = ?2 AND expires_at > unixepoch())",
                params![self.id.to_string(), self.owner.to_string()],
                |row| row.get(0),
            )
            .map_err(query)?;
        if held { Ok(()) } else { Err(lost()) }
    }

    fn release(self, tx: &Transaction<'_>) -> Result<(), EntityStoreError> {
        tx.execute(
            "DELETE FROM rh_entity_leases WHERE entity_id = ?1 AND owner = ?2",
            params![self.id.to_string(), self.owner.to_string()],
        )
        .map_err(query)?;
        Ok(())
    }
}

pub(super) struct Guard {
    store: SqliteStore,
    key: Option<Key>,
    runtime: Handle,
    heartbeat: Option<JoinHandle<()>>,
}

impl Guard {
    pub(super) async fn acquire(store: &SqliteStore, id: Uuid) -> Result<Self, EntityStoreError> {
        let key = Key::new(id);
        loop {
            let guard_store = store.clone();
            let runtime = Handle::current();
            let claimed = write(store, move |tx| {
                if !key.claim(tx, clock(tx)?)? {
                    return Ok(None);
                }
                Ok(Some(Self {
                    store: guard_store,
                    key: Some(key),
                    runtime,
                    heartbeat: None,
                }))
            })
            .await?;
            if let Some(mut guard) = claimed {
                let next = Instant::now() + HEARTBEAT;
                guard.heartbeat = Some(tokio::spawn(renew(guard.store.clone(), key, next)));
                return Ok(guard);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub(super) fn key(&self) -> Result<Key, EntityStoreError> {
        self.key.ok_or_else(lost)
    }

    pub(super) async fn release(&mut self) -> Result<(), EntityStoreError> {
        if let Some(key) = self.key {
            write(&self.store, move |tx| key.release(tx)).await?;
            self.key = None;
            self.stop();
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
        if let Some(key) = self.key.take() {
            let store = self.store.clone();
            self.runtime.spawn(async move {
                if let Err(error) = write(&store, move |tx| key.release(tx)).await {
                    tracing::warn!(%error, entity = %key.id, "entity lease will expire");
                }
            });
        }
    }
}

async fn renew(store: SqliteStore, key: Key, mut next: Instant) {
    loop {
        tokio::time::sleep_until(next).await;
        match write(&store, move |tx| key.renew(tx)).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => tracing::warn!(%error, entity = %key.id, "entity lease renewal failed"),
        }
        next = Instant::now() + HEARTBEAT;
    }
}

pub(super) async fn scoped<T, F>(
    store: &SqliteStore,
    ids: impl IntoIterator<Item = Uuid>,
    mut body: F,
) -> Result<T, EntityStoreError>
where
    T: Send + 'static,
    F: FnOnce(&Transaction<'_>) -> Result<T, EntityStoreError> + Send + 'static,
{
    let keys: Vec<_> = ids.into_iter().map(Key::new).collect();
    loop {
        let keys = keys.clone();
        let attempt = write(store, move |tx| {
            let now = clock(tx)?;
            for key in &keys {
                if !key.available(tx, now)? {
                    return Ok(Err(body));
                }
            }
            for key in &keys {
                if !key.claim(tx, now)? {
                    return Err(lost());
                }
            }
            let result = body(tx)?;
            for key in keys {
                key.release(tx)?;
            }
            Ok(Ok(result))
        })
        .await?;
        match attempt {
            Ok(result) => return Ok(result),
            Err(retry) => body = retry,
        }
        tokio::time::sleep(POLL).await;
    }
}

fn clock(tx: &Transaction<'_>) -> Result<i64, EntityStoreError> {
    tx.query_row("SELECT unixepoch()", [], |row| row.get(0))
        .map_err(query)
}

fn lost() -> EntityStoreError {
    error(std::io::Error::other("entity lease is no longer held"))
}
