mod append;
mod bundle;
pub mod capture;
mod changes;
mod chunk_rows;
mod conversation;
mod deletion;
mod digest;
mod embed_store;
pub mod error;
pub mod export;
mod facts;
mod flagged;
pub mod import;
mod insert;
mod job_notify;
mod job_rank;
mod memory_jobs;
mod memory_port;
mod message;
mod meta;
mod model_calls;
mod page_files;
mod port;
mod queue;
mod read;
mod reembed;
mod review;
mod search;
mod session_catalog;
mod session_marks;
mod session_messages;
pub use search::embed::QueryEmbed;
mod cleanup;
mod queue_admin;
mod settlement;
mod stats;
mod summary_backend;
mod summary_conversion;
mod summary_dag;
mod summary_index;
mod summary_opening;
mod summary_port;
mod summary_rehearsal;
mod summary_replace;
mod tags;
mod tags_suggest;
mod tags_write;
mod taxonomy;
mod tenancy;
mod tidy;
mod time;
mod title;
mod topic_pages;
mod topic_write;
mod topics;
mod trigrams;
mod unembedded;
mod vector;
mod wiki_archive;
mod wiki_builder;
mod wiki_citations;
mod wiki_consolidate;
mod wiki_held;
mod wiki_history;
mod wiki_materialize;
mod wiki_page_files;
mod wiki_read;
mod wiki_topics;
mod worker_pauses;

use jiff::Timestamp;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;

use kernel::queue::JobId;
use parking_lot::Mutex;
use rusqlite::Connection;

pub use capture::MAX_CONTENT_CHARS;
pub use error::SqliteError;
#[doc(hidden)]
pub use flagged::archive_for_test;
pub use page_files::set_lock_pause;
#[doc(hidden)]
pub use page_files::set_row_pause;
pub use queue::ClaimedSqliteJob;
pub use queue::FailOutcome;
pub use queue::JobInsert;
pub use queue::JobPayload;
pub use queue::JobState;
pub use reembed::ReembedReport;

#[derive(Clone)]
struct SharedClock(Arc<dyn Fn() -> Timestamp + Send + Sync>);

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for SharedClock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("SharedClock").finish_non_exhaustive()
    }
}

impl SharedClock {
    fn new(clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        Self(Arc::new(clock))
    }

    fn now(&self) -> Timestamp {
        (self.0)()
    }
}

pub const SCHEMA_VERSION: i64 = 47;

mod entity_builder;

const FILE_PRAGMAS: &str = "PRAGMA journal_mode = wal;\nPRAGMA foreign_keys = ON;";
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn prepare_conn(conn: &Connection) -> Result<(), SqliteError> {
    conn.execute_batch(FILE_PRAGMAS)
        .map_err(SqliteError::Pragma)?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(SqliteError::Pragma)?;
    register(conn)
}

pub(crate) fn open_writer(path: &Path) -> Result<Connection, SqliteError> {
    let conn = Connection::open(path).map_err(|source| SqliteError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    prepare_conn(&conn)?;
    Ok(conn)
}

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
    language_available: bool,
    claims: Arc<ClaimIds>,
    clock: SharedClock,
    query_embed: Option<QueryEmbed>,
    embed_model: Option<String>,
    summary_index_key: Option<[u8; 32]>,
    wiki_directory: Option<Arc<PathBuf>>,
    notifications: tokio::sync::watch::Sender<()>,
    pause_watch: Arc<job_notify::PauseWatch>,
    summaries_written: tokio::sync::broadcast::Sender<kernel::memory::WrittenSummary>,
}

const SUMMARY_WRITTEN_BUFFER: usize = 256;

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteError> {
        opened(path.as_ref(), false)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, SqliteError> {
        opened(path.as_ref(), true)
    }

    #[must_use]
    pub fn with_summary_index_key(mut self, key: [u8; 32]) -> Self {
        self.summary_index_key = Some(key);
        self
    }

    #[must_use]
    pub fn with_language_available(mut self, available: bool) -> Self {
        self.language_available = available;
        self
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
    pub fn with_wiki_directory(mut self, directory: PathBuf) -> Self {
        self.wiki_directory = Some(Arc::new(directory));
        self
    }

    #[must_use]
    pub fn wiki_directory(&self) -> Option<&Path> {
        self.wiki_directory.as_deref().map(PathBuf::as_path)
    }

    #[must_use]
    pub fn with_clock(self, clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
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

    pub(crate) fn now(&self) -> Timestamp {
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

fn opened(path: &Path, read_only: bool) -> Result<SqliteStore, SqliteError> {
    if !path.exists() {
        return Err(SqliteError::NotInitialised {
            path: path.to_path_buf(),
        });
    }
    let found = read_user_version(path)?;
    verify(path, found)?;
    let conn = open_file(path, read_only)?;
    if read_only {
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(SqliteError::Pragma)?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(SqliteError::Pragma)?;
        register(&conn)?;
    } else {
        prepare_conn(&conn)?;
    }
    Ok(from_conn(conn, SharedClock::new(Timestamp::now)))
}

fn open_file(path: &Path, read_only: bool) -> Result<Connection, SqliteError> {
    let opened = if read_only {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    } else {
        Connection::open(path)
    };
    opened.map_err(|source| SqliteError::Open {
        path: path.to_path_buf(),
        source,
    })
}

fn register(conn: &Connection) -> Result<(), SqliteError> {
    search::functions::register(conn)?;
    session_catalog::register(conn)?;
    time::register(conn)?;
    entity_builder::register(conn)?;
    Ok(())
}

fn from_conn(conn: Connection, clock: SharedClock) -> SqliteStore {
    SqliteStore {
        conn: Arc::new(Mutex::new(conn)),
        pending: tokio_util::task::TaskTracker::new(),
        logs: Arc::new(Mutex::new(Vec::new())),
        embed: Arc::new(AtomicBool::new(false)),
        language_available: false,
        claims: Arc::new(ClaimIds::default()),
        clock,
        query_embed: None,
        embed_model: None,
        summary_index_key: None,
        wiki_directory: None,
        notifications: tokio::sync::watch::channel(()).0,
        pause_watch: Arc::new(job_notify::PauseWatch::new()),
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
}
