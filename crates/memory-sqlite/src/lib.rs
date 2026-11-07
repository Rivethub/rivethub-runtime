mod append;
mod bundle;
pub mod capture;
mod context;
mod conversation;
mod deletion;
mod embed_store;
pub mod error;
pub mod export;
mod flagged;
mod health;
pub mod import;
mod insert;
mod job_rank;
mod memory_port;
mod message;
mod meta;
mod port;
mod queue;
mod read;
mod reembed;
mod search;
mod session_catalog;
mod session_marks;
pub use search::embed::QueryEmbed;
mod stats;
mod summary_dag;
mod summary_port;
mod tags;
mod tags_suggest;
mod tags_write;
mod task_rows;
mod task_store;
mod task_watch;
mod taxonomy;
mod tenancy;
mod tidy;
mod time;
mod title;
mod topic_pages;
mod topics;
mod unembedded;
mod vector;
mod wiki_builder;
mod wiki_citations;
mod wiki_consolidate;
mod wiki_held;
mod wiki_read;
mod wiki_topics;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use chrono::DateTime;
use chrono::Utc;
use kernel::queue::JobId;
use parking_lot::Mutex;
use rusqlite::Connection;

pub use capture::MAX_CONTENT_CHARS;
pub use error::SqliteError;
pub use health::SqliteHealth;
pub use queue::ClaimedSqliteJob;
pub use queue::FailOutcome;
pub use queue::JobCount;
pub use queue::JobInsert;
pub use queue::JobPayload;
pub use queue::JobState;
pub use queue::retry_delay_ms;
pub use reembed::ReembedReport;

#[derive(Clone)]
struct SharedClock(Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "derive(Debug) cannot format a function object; the impl names the clock and skips the closure"
    )
)]
impl std::fmt::Debug for SharedClock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("SharedClock").finish_non_exhaustive()
    }
}

impl SharedClock {
    fn new(clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Self(Arc::new(clock))
    }

    fn now(&self) -> DateTime<Utc> {
        (self.0)()
    }
}

pub const SCHEMA_VERSION: i64 = 31;

const FILE_PRAGMAS: &str = "PRAGMA journal_mode = wal;\nPRAGMA foreign_keys = ON;";

#[derive(Debug, Default)]
struct ClaimIds {
    next: AtomicI64,
    text: Mutex<BTreeMap<i64, String>>,
}

#[derive(Debug, Clone)]
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    pending: tokio_util::task::TaskTracker,
    logs: Arc<Mutex<Vec<String>>>,
    embed: Arc<AtomicBool>,
    claims: Arc<ClaimIds>,
    clock: SharedClock,
    query_embed: Option<QueryEmbed>,
    embed_model: Option<String>,
    notifications: tokio::sync::watch::Sender<()>,
    task_events: tokio::sync::watch::Sender<u64>,
    task_finished: tokio::sync::broadcast::Sender<uuid::Uuid>,
    summaries_written: tokio::sync::broadcast::Sender<kernel::memory::WrittenSummary>,
}

const TASK_FINISHED_BUFFER: usize = 256;
const SUMMARY_WRITTEN_BUFFER: usize = 256;

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(SqliteError::NotInitialised {
                path: path.to_path_buf(),
            });
        }
        let found = read_user_version(path)?;
        verify(path, found)?;
        let conn = Connection::open(path).map_err(|source| SqliteError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(FILE_PRAGMAS)
            .map_err(SqliteError::Pragma)?;
        search::functions::register(&conn)?;
        let task_events = tokio::sync::watch::channel(0).0;
        let task_finished = tokio::sync::broadcast::channel(TASK_FINISHED_BUFFER).0;
        task_watch::register(&conn, task_events.clone(), task_finished.clone())?;
        Ok(from_conn(
            conn,
            SharedClock::new(Utc::now),
            task_events,
            task_finished,
        ))
    }

    #[must_use]
    pub fn with_query_embed(mut self, embed: QueryEmbed) -> Self {
        self.query_embed = Some(embed);
        self
    }

    #[must_use]
    pub fn with_embed_model(mut self, model: &str) -> Self {
        self.embed_model = Some(model.to_owned());
        self
    }

    #[must_use]
    pub fn with_clock(self, clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Self {
            clock: SharedClock::new(clock),
            ..self
        }
    }

    pub async fn with_conn<T, F>(&self, f: F) -> Result<T, SqliteError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, SqliteError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        match self
            .pending
            .spawn_blocking(move || f(&mut conn.lock()))
            .await
        {
            Ok(result) => result,
            Err(source) => Err(SqliteError::Join(source)),
        }
    }

    pub fn record_log(&self, line: impl AsRef<str>) {
        let line = line.as_ref().to_owned();
        tracing::info!("{line}");
        self.logs.lock().push(line);
    }

    #[must_use]
    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().clone()
    }

    pub fn arm_embed(&self) {
        self.embed.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn embed_armed(&self) -> bool {
        self.embed.load(Ordering::Acquire)
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) fn remember_claim(&self, text_id: &str) -> Result<JobId, SqliteError> {
        self.claims.issue(text_id)
    }

    pub(crate) fn text_for_claim(&self, id: JobId) -> Result<String, SqliteError> {
        self.claims.text(id)
    }
}

impl ClaimIds {
    fn issue(&self, text_id: &str) -> Result<JobId, SqliteError> {
        let issued = loop {
            let current = self.next.load(Ordering::Acquire);
            let Some(next) = current.checked_add(1) else {
                return Err(SqliteError::Clock);
            };
            if self
                .next
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break current;
            }
        };
        self.text.lock().insert(issued, text_id.to_owned());
        Ok(JobId::new(issued))
    }

    fn text(&self, id: JobId) -> Result<String, SqliteError> {
        let key = i64::from(id);
        self.text
            .lock()
            .get(&key)
            .cloned()
            .ok_or_else(|| SqliteError::MissingJob {
                id: key.to_string(),
            })
    }
}

fn from_conn(
    conn: Connection,
    clock: SharedClock,
    task_events: tokio::sync::watch::Sender<u64>,
    task_finished: tokio::sync::broadcast::Sender<uuid::Uuid>,
) -> SqliteStore {
    SqliteStore {
        conn: Arc::new(Mutex::new(conn)),
        pending: tokio_util::task::TaskTracker::new(),
        logs: Arc::new(Mutex::new(Vec::new())),
        embed: Arc::new(AtomicBool::new(false)),
        claims: Arc::new(ClaimIds::default()),
        clock,
        query_embed: None,
        embed_model: None,
        notifications: tokio::sync::watch::channel(()).0,
        task_events,
        task_finished,
        summaries_written: tokio::sync::broadcast::channel(SUMMARY_WRITTEN_BUFFER).0,
    }
}

fn read_user_version(path: &Path) -> Result<i64, SqliteError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|source| SqliteError::Open {
            path: path.to_path_buf(),
            source,
        })?;
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(SqliteError::Pragma)
}

fn verify(path: &Path, found: i64) -> Result<(), SqliteError> {
    match found {
        0 => Err(SqliteError::NotInitialised {
            path: path.to_path_buf(),
        }),
        SCHEMA_VERSION => Ok(()),
        unsupported if !(0..=SCHEMA_VERSION).contains(&unsupported) => {
            Err(SqliteError::UnsupportedSchema { found: unsupported })
        }
        behind => Err(SqliteError::SchemaBehind {
            path: path.to_path_buf(),
            found: behind,
            required: SCHEMA_VERSION,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteStore;
    use crate::error::SqliteError;

    #[tokio::test]
    async fn with_conn_runs_off_the_async_worker() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path).unwrap();
        let store = SqliteStore::open(&path).unwrap();
        let awaited_on = std::thread::current().id();
        let ran_on = store
            .with_conn(move |_conn| {
                assert!(tokio::runtime::Handle::try_current().is_ok());
                Ok(std::thread::current().id())
            })
            .await
            .unwrap();
        assert_ne!(ran_on, awaited_on);
    }

    #[tokio::test]
    async fn migrated_store_opens_the_current_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path).unwrap();
        let store = SqliteStore::open(&path).unwrap();
        let state = store
            .with_conn(move |conn| {
                let version: i64 = conn
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                let tables: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                Ok((version, tables))
            })
            .await
            .unwrap();
        assert_eq!(state, (crate::SCHEMA_VERSION, 71));
    }

    #[tokio::test]
    async fn with_conn_propagates_the_closure_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path).unwrap();
        let store = SqliteStore::open(&path).unwrap();
        let outcome = store
            .with_conn(move |_conn| Err::<i64, _>(SqliteError::UnsupportedSchema { found: 9 }))
            .await;
        match outcome {
            Err(SqliteError::UnsupportedSchema { found }) => assert_eq!(found, 9),
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }
}
