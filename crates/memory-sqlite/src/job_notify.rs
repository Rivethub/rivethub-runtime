use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use kernel::queue::JobNotifications;
use kernel::queue::JobQueueError;
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::SqliteStore;

const PAUSE_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
struct PauseSample {
    version: i64,
    fingerprint: String,
}

#[derive(Debug)]
pub(crate) struct PauseWatch {
    started: AtomicBool,
    baseline: Mutex<Option<PauseSample>>,
}

impl PauseWatch {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            baseline: Mutex::new(None),
        }
    }
}

pub(crate) struct StoreNotifications {
    updates: tokio::sync::watch::Receiver<()>,
}

impl StoreNotifications {
    pub(crate) async fn open(store: &SqliteStore) -> Result<Self, JobQueueError> {
        start_poller(store).await?;
        Ok(Self {
            updates: store.notifications.subscribe(),
        })
    }
}

#[async_trait]
impl JobNotifications for StoreNotifications {
    async fn recv(&mut self) -> Result<(), JobQueueError> {
        self.updates
            .changed()
            .await
            .map_err(|source| JobQueueError::Unavailable(Box::new(source)))
    }
}

pub(crate) async fn record_baseline(store: &SqliteStore) -> Result<(), JobQueueError> {
    let sample = pause_sample(&store.conn).await?;
    *store.pause_watch.baseline.lock() = Some(sample);
    Ok(())
}

async fn start_poller(store: &SqliteStore) -> Result<(), JobQueueError> {
    if store.pause_watch.started.load(Ordering::Acquire) {
        return Ok(());
    }
    let sample = pause_sample(&store.conn).await?;
    let mut slot = store.pause_watch.baseline.lock();
    if slot.is_none() {
        *slot = Some(sample);
    }
    if store.pause_watch.started.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    drop(slot);
    let watch = Arc::downgrade(&store.pause_watch);
    let conn = Arc::clone(&store.conn);
    let notifications = store.notifications.clone();
    tokio::spawn(async move {
        poll_pauses(watch, conn, notifications).await;
    });
    Ok(())
}

async fn poll_pauses(
    watch: Weak<PauseWatch>,
    conn: Arc<Mutex<Connection>>,
    notifications: tokio::sync::watch::Sender<()>,
) {
    loop {
        tokio::time::sleep(PAUSE_POLL).await;
        let Some(gate) = watch.upgrade() else {
            return;
        };
        let Some(before) = gate.baseline.lock().clone() else {
            continue;
        };
        let sample = match pause_sample(&conn).await {
            Ok(sample) => sample,
            Err(error) => {
                tracing::warn!("sqlite pause poll failed: {error}");
                continue;
            }
        };
        let mut slot = gate.baseline.lock();
        let Some(current) = slot.clone() else {
            continue;
        };
        if current != before {
            continue;
        }
        if sample.fingerprint != current.fingerprint {
            notifications.send_replace(());
        }
        *slot = Some(sample);
    }
}

async fn pause_sample(conn: &Arc<Mutex<Connection>>) -> Result<PauseSample, JobQueueError> {
    let version = read_i64(conn, "PRAGMA data_version", "data version").await?;
    let fingerprint = read_text(
        conn,
        "SELECT role, paused_at, ifnull(paused_by, '') FROM rh_worker_pauses ORDER BY role",
        "pause table",
    )
    .await?;
    Ok(PauseSample {
        version,
        fingerprint,
    })
}

async fn read_i64(
    conn: &Arc<Mutex<Connection>>,
    sql: &'static str,
    operation: &'static str,
) -> Result<i64, JobQueueError> {
    let conn = Arc::clone(conn);
    match tokio::task::spawn_blocking(move || {
        conn.lock()
            .query_row(sql, [], |row| row.get(0))
            .map_err(|source| operation_error(operation, source))
    })
    .await
    {
        Ok(result) => result,
        Err(source) => Err(JobQueueError::Unavailable(Box::new(source))),
    }
}

async fn read_text(
    conn: &Arc<Mutex<Connection>>,
    sql: &'static str,
    operation: &'static str,
) -> Result<String, JobQueueError> {
    let conn = Arc::clone(conn);
    match tokio::task::spawn_blocking(move || fingerprint(&conn, sql, operation)).await {
        Ok(result) => result,
        Err(source) => Err(JobQueueError::Unavailable(Box::new(source))),
    }
}

fn fingerprint(
    conn: &Mutex<Connection>,
    sql: &str,
    operation: &'static str,
) -> Result<String, JobQueueError> {
    let locked = conn.lock();
    let text = pause_rows(&locked, sql, operation)?;
    drop(locked);
    Ok(text)
}

fn pause_rows(
    conn: &Connection,
    sql: &str,
    operation: &'static str,
) -> Result<String, JobQueueError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|source| operation_error(operation, source))?;
    let rows = statement
        .query_map([], |row| {
            let role: String = row.get(0)?;
            let paused_at: String = row.get(1)?;
            let paused_by: String = row.get(2)?;
            Ok(format!("{role}\n{paused_at}\n{paused_by}\n"))
        })
        .map_err(|source| operation_error(operation, source))?;
    let mut text = String::new();
    for row in rows {
        text.push_str(&row.map_err(|source| operation_error(operation, source))?);
    }
    Ok(text)
}

fn operation_error(operation: &'static str, source: rusqlite::Error) -> JobQueueError {
    JobQueueError::Operation {
        operation,
        source: Box::new(source),
    }
}
