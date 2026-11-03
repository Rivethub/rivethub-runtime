mod access;
pub mod admin;
mod agent_presets;
mod backfills;
pub mod capture;
mod cleanup;
mod conversation;
mod deletion;
pub mod dump;
pub mod embed;
mod embedding_store;
pub mod error;
mod flagged;
mod graphile;
mod graphile_sql;
mod hub_state;
mod listen;
mod loss;
mod memory_jobs;
pub mod merge;
pub mod queue;
pub mod read;
pub mod search;
mod search_cap;
mod search_complete;
mod search_content;
mod search_details;
mod search_recent;
mod search_sql;
mod search_vector;
mod stats;
pub use search_complete::SearchSynthesis;
mod stats_cache;
mod store_impl;
mod summary_dag;
mod summary_store;
mod summary_watch;
mod tags;
mod task_db;
mod task_jobs;
mod task_rows;
mod task_watch;
mod tenancy;
mod tidy;
mod title;
mod topics;
mod wiki;
mod wiki_builder;
mod wiki_citations;
mod wiki_scope;
pub mod wiki_writer;

#[cfg(test)]
mod resilience;
mod scope;
pub use scope::scoped;

pub use agent_presets::PostgresPresets;
pub use cleanup::DatabaseIdentity;
pub use hub_state::PostgresHubState;
pub use task_db::PostgresTasks;
pub use tenancy::PostgresTenancy;
pub use title::Untitled;

use std::cmp::Ordering;
use std::time::Duration;

pub use capture::CaptureOptions;
pub use capture::CaptureWriter;
pub use capture::ConversationEnsure;
pub use capture::ConversationRecord;
pub use capture::ConversationUpdate;
pub use capture::ConversationUpsert;
pub use capture::MessageRow;
pub use capture::UpsertedConversation;
pub use embed::QueryEmbed;
pub use error::Error;
use graphile::GraphileQueue;
pub use queue::PostgresQueue;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub const SCHEMA_VERSION: i64 = 52;
pub(crate) const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct PostgresMemory {
    wiki_directory: Option<std::path::PathBuf>,
    wiki_owner: Option<kernel::owner::Owner>,
    pool: PgPool,
    embed: Option<embed::EmbedArm>,
    synthesis: Option<SearchSynthesis>,
    access: std::sync::Arc<access::AccessLog>,
    stats_cache: std::sync::Arc<stats_cache::StatsCache>,
    backfills: std::sync::Arc<backfills::Backfills>,
    unverified: bool,
    summaries_written: tokio::sync::broadcast::Sender<kernel::memory::WrittenSummary>,
}

const SUMMARY_WRITTEN_BUFFER: usize = 256;

impl PostgresMemory {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            wiki_directory: None,
            wiki_owner: None,
            pool,
            embed: None,
            synthesis: None,
            access: std::sync::Arc::default(),
            stats_cache: std::sync::Arc::default(),
            backfills: std::sync::Arc::default(),
            unverified: false,
            summaries_written: tokio::sync::broadcast::channel(SUMMARY_WRITTEN_BUFFER).0,
        }
    }

    #[must_use]
    pub fn with_wiki_directory(mut self, directory: std::path::PathBuf) -> Self {
        self.wiki_directory = Some(directory);
        self
    }

    #[must_use]
    pub fn with_wiki_owner(mut self, owner: kernel::owner::Owner) -> Self {
        self.wiki_owner = Some(owner);
        self
    }

    #[must_use]
    pub fn with_query_embed(mut self, embed: QueryEmbed) -> Self {
        self.embed = Some(embed::EmbedArm::new(embed));
        self
    }

    #[must_use]
    pub fn with_search_synthesis(mut self, synthesis: SearchSynthesis) -> Self {
        self.synthesis = Some(synthesis);
        self
    }

    pub async fn connect(url: &str) -> Result<Self, Error> {
        let pool = open_pool(url, POOL_CONNECTIONS, Some(Pooling::OnDemand.idle()))
            .await
            .map_err(Error::Connection)?;
        Self::prepare(&pool).await?;
        Ok(Self::new(pool))
    }

    pub async fn connect_deferred(url: &str, pooling: Pooling) -> Result<Self, Error> {
        let options = pool_options(POOL_CONNECTIONS, Some(pooling.idle()));
        match options.connect(url).await {
            Ok(pool) => {
                Self::prepare(&pool).await?;
                Ok(Self::new(pool))
            }
            Err(error) if loss::unreachable(&error) => {
                tracing::warn!(
                    %error,
                    "the memory database is unreachable; memory stays unavailable until it answers"
                );
                let pool = lazy_pool(url, POOL_CONNECTIONS, Some(pooling.idle()))
                    .map_err(Error::Connection)?;
                tokio::spawn(check_when_reachable(pool.clone()));
                Ok(Self {
                    unverified: true,
                    ..Self::new(pool)
                })
            }
            Err(error) => Err(Error::Connection(error)),
        }
    }

    pub async fn prepare(pool: &PgPool) -> Result<(), Error> {
        let found = newest_applied(pool).await?;
        match found.cmp(&SCHEMA_VERSION) {
            Ordering::Equal => Ok(()),
            Ordering::Greater => {
                tracing::warn!(
                    "the memory schema reports version {found}, this build requires {SCHEMA_VERSION} — rolling updates migrate first; continuing"
                );
                Ok(())
            }
            Ordering::Less => Err(Error::SchemaBehind {
                found,
                required: SCHEMA_VERSION,
            }),
        }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) async fn ping(&self) -> Result<(), Error> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| Error::operation("probe", source))
    }

    pub async fn queue(&self) -> Result<PostgresQueue, Error> {
        PostgresQueue::connect(self.pool.clone()).await
    }

    pub async fn queue_available(&self) -> Result<bool, Error> {
        GraphileQueue::schema_present(&self.pool).await
    }
}

const RECHECK: Duration = Duration::from_secs(5);
const POOL_CONNECTIONS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    OnDemand,
    Serving,
}

impl Pooling {
    const fn idle(self) -> Duration {
        match self {
            Self::OnDemand => Duration::from_secs(30),
            Self::Serving => Duration::from_secs(600),
        }
    }
}

async fn check_when_reachable(pool: PgPool) {
    loop {
        tokio::time::sleep(RECHECK).await;
        match PostgresMemory::prepare(&pool).await {
            Ok(()) => {
                tracing::info!("the memory database answered; memory is available");
                return;
            }
            Err(error @ (Error::SchemaBehind { .. } | Error::NotAdopted)) => {
                tracing::error!(%error, "the memory database answered but cannot serve this build");
                return;
            }
            Err(error) => tracing::debug!(%error, "the memory database is still unreachable"),
        }
    }
}

fn pool_options(max_connections: u32, idle_timeout: Option<Duration>) -> PgPoolOptions {
    let options = scoped(
        PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .test_before_acquire(false),
    );
    match idle_timeout {
        Some(idle) => options.idle_timeout(idle),
        None => options,
    }
}

pub(crate) fn lazy_pool(
    url: &str,
    max_connections: u32,
    idle_timeout: Option<Duration>,
) -> Result<PgPool, sqlx::Error> {
    pool_options(max_connections, idle_timeout).connect_lazy(url)
}

pub(crate) async fn open_pool(
    url: &str,
    max_connections: u32,
    idle_timeout: Option<Duration>,
) -> Result<PgPool, sqlx::Error> {
    const ATTEMPTS: u8 = 3;
    let mut attempt = 1;
    loop {
        match pool_options(max_connections, idle_timeout)
            .connect(url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(sqlx::Error::PoolTimedOut) if attempt < ATTEMPTS => {
                tracing::warn!(
                    "memory database connection timed out; retrying (attempt {attempt} of {ATTEMPTS})"
                );
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn newest_applied(pool: &PgPool) -> Result<i64, Error> {
    match sqlx::query!(
        "select coalesce(max(version), 0) as \"applied!\" from _sqlx_migrations where success"
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => Ok(row.applied),
        Err(source) if Error::relation_missing(&source) => Err(Error::NotAdopted),
        Err(source) => Err(Error::operation("schema version check", source)),
    }
}
