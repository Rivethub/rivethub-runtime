use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use kernel::wiki::builder::WikiStoreError;
use parking_lot::Mutex;
use wiki_files::wiki_writer::LockedRepository;
use wiki_files::wiki_writer::WikiRepository;

type RowPause = Arc<dyn Fn() + Send + Sync>;

static ROW_PAUSE: LazyLock<Mutex<Option<RowPause>>> = LazyLock::new(Mutex::default);
static LOCK_PAUSE: LazyLock<Mutex<Option<RowPause>>> = LazyLock::new(Mutex::default);

pub(crate) fn pause_before_row() {
    let hook = ROW_PAUSE.lock().clone();
    if let Some(hook) = hook {
        hook();
    }
}

pub(crate) async fn blocking_locked<T, F>(
    locked: LockedRepository,
    body: F,
) -> Result<T, WikiStoreError>
where
    T: Send + 'static,
    F: FnOnce(&mut LockedRepository) -> Result<T, WikiStoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut locked = locked;
        let result = body(&mut locked);
        tokio::runtime::Handle::current().block_on(finish(locked, result))
    })
    .await
    .map_err(|error| WikiStoreError::Query(Box::new(std::io::Error::other(error.to_string()))))?
}

pub(crate) struct HeldTx {
    conn: rusqlite::Connection,
}

impl HeldTx {
    pub(crate) fn open(path: &Path) -> Result<Self, WikiStoreError> {
        let conn =
            crate::open_writer(path).map_err(|error| WikiStoreError::Query(Box::new(error)))?;
        Ok(Self { conn })
    }

    pub(crate) fn immediate(&mut self) -> Result<rusqlite::Transaction<'_>, WikiStoreError> {
        self.conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| WikiStoreError::Query(Box::new(error)))
    }
}

pub(crate) async fn database_path(store: &crate::SqliteStore) -> Result<PathBuf, WikiStoreError> {
    let file = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| crate::SqliteError::Query {
                operation: "read database file",
                source,
            })
        })
        .await
        .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
    Ok(PathBuf::from(file))
}

async fn pause_hook(slot: &Mutex<Option<RowPause>>) -> Result<(), WikiStoreError> {
    let Some(hook) = slot.lock().clone() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || hook())
        .await
        .map_err(|error| WikiStoreError::Query(Box::new(std::io::Error::other(error.to_string()))))
}

#[doc(hidden)]
pub fn set_row_pause(hook: Option<RowPause>) {
    *ROW_PAUSE.lock() = hook;
}

#[doc(hidden)]
pub fn set_lock_pause(hook: Option<RowPause>) {
    *LOCK_PAUSE.lock() = hook;
}

pub(crate) struct Files {
    root: PathBuf,
    entity: bool,
}

impl Files {
    pub(crate) fn wiki(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            entity: false,
        }
    }

    pub(crate) fn entity(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            entity: true,
        }
    }

    pub(crate) async fn lock(&self) -> Result<LockedRepository, WikiStoreError> {
        pause_hook(&LOCK_PAUSE).await?;
        let repository = if self.entity {
            WikiRepository::new(self.root.clone())
        } else {
            WikiRepository::for_store(self.root.clone())
        };
        repository.lock().await.map_err(WikiStoreError::from)
    }
}

pub(crate) async fn finish<T>(
    locked: LockedRepository,
    result: Result<T, WikiStoreError>,
) -> Result<T, WikiStoreError> {
    let released = locked.release().await.map_err(WikiStoreError::from);
    match (result, released) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(release)) => {
            tracing::warn!(%release, "wiki repository release failed");
            Err(error)
        }
    }
}
