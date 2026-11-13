mod account;
pub(crate) mod identities;

mod models;
pub(crate) mod plugins;
mod spool;

pub use capture::listing::store_roots;

use std::sync::Arc;

use cli::memory::MemoryExporter;
use cli::memory::SessionStore;
use config::{Config, Environment};
use gateway::Gateway;
use gateway::sessions::SessionRegistry;
use kernel::filing::FilingStore;
use kernel::memory::MemoryExport;
use kernel::memory::MemoryStore;
use kernel::tidy::TidyStore;
use kernel::tidy::UpkeepStore;
use kernel::topics::DecisionStore;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use thiserror::Error;

use crate::share_receive::NodeReceiver;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::share_desk::NodeDesk;

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Node(#[from] crate::NodeError),
    #[error(transparent)]
    Settings(#[from] gateway::SettingsError),
    #[error("gateway composition I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("gateway composition worker failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("gateway configuration is invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Api(#[from] gateway::error::ApiError),
    #[error(transparent)]
    Provider(#[from] crate::models::ModelError),
    #[error("gateway configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Bind(#[from] gateway::server::BindError),
    #[error(transparent)]
    Plugins(#[from] gateway::plugins::HostError),
    #[error(transparent)]
    Memory(#[from] memory_postgres::Error),
    #[error(transparent)]
    Account(#[from] gateway::account::AccountError),
    #[error(transparent)]
    LocalOwner(#[from] gateway::local_owner::TokenError),
    #[error("gateway.drops cannot start: {0}")]
    Drops(#[from] gateway::drops::OpenError),
}

pub(crate) async fn local_owner(
    state_dir: &std::path::Path,
) -> Result<Arc<gateway::local_owner::LocalOwner>, ComposeError> {
    let dir = state_dir.to_path_buf();
    let holder =
        tokio::task::spawn_blocking(move || gateway::local_owner::LocalOwner::load_or_create(&dir))
            .await??;
    Ok(Arc::new(holder))
}

#[expect(
    missing_debug_implementations,
    reason = "Gateway contains kernel ports without Debug"
)]
pub struct Composition {
    gateway: Arc<Gateway>,
    tasks: JoinSet<Result<(), ComposeError>>,
    background: tokio_util::task::TaskTracker,
    model_tasks: crate::models::ModelTasks,
    cancel: CancellationToken,
}

fn gateway_settings(config: &Config, env: &Environment) -> Result<gateway::Settings, ComposeError> {
    let node = crate::identity::announce_resolved("gateway", config, env)?;
    Ok(gateway::Settings::from_config_in(
        config,
        env,
        node.hostname,
        node.name,
    )?)
}

#[derive(Default)]
struct Backing {
    export: Option<Arc<dyn MemoryExport>>,
    topics: Option<(Arc<dyn TopicStore>, Arc<dyn FilingStore>)>,
    decisions: Option<Arc<dyn DecisionStore>>,
    facts: Option<Arc<dyn kernel::facts::FactStore>>,
    review: Option<Arc<dyn kernel::review::ReviewStore>>,
    pages: Option<Arc<dyn TopicPageStore>>,
    tidy: Option<(Arc<dyn TidyStore>, Arc<dyn UpkeepStore>)>,
    flagged: Option<Arc<dyn kernel::flagged::FlaggedJobs>>,
    summaries: Option<Arc<dyn kernel::memory::SummaryWatch>>,
    deletions: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    catalog: Option<Arc<dyn kernel::memory::SessionCatalog>>,
    changes: Option<(
        Arc<dyn kernel::changes::ChangeFeed>,
        Arc<dyn kernel::changes::DigestStore>,
    )>,
    model_calls: Option<Arc<dyn kernel::model_calls::ModelCallStore>>,
    tenancy: Option<Arc<dyn kernel::tenancy::TenancyStore>>,
    sessions: Option<SessionStore>,
}

fn with_backing(gateway: Gateway, backing: &Backing) -> Gateway {
    let gateway = match backing.export.clone() {
        Some(export) => gateway.with_memory_export(export),
        None => gateway,
    };
    let gateway = match backing.topics.clone() {
        Some((topics, filing)) => gateway.with_topics(topics, filing),
        None => gateway,
    };
    let gateway = match backing.decisions.clone() {
        Some(decisions) => gateway.with_decisions(decisions),
        None => gateway,
    };
    let gateway = match backing.facts.clone() {
        Some(facts) => gateway.with_facts(facts),
        None => gateway,
    };
    let gateway = match backing.review.clone() {
        Some(review) => gateway.with_review(review),
        None => gateway,
    };
    let gateway = match backing.tidy.clone() {
        Some((tidy, upkeep)) => gateway.with_tidy(tidy, upkeep),
        None => gateway,
    };
    let gateway = match backing.flagged.clone() {
        Some(flagged) => gateway.with_flagged(flagged),
        None => gateway,
    };
    let gateway = match backing.summaries.clone() {
        Some(watch) => gateway.with_summary_watch(watch),
        None => gateway,
    };
    let gateway = match backing.deletions.clone() {
        Some(deletions) => gateway.with_session_deletion(deletions),
        None => gateway,
    };
    let gateway = match backing.catalog.clone() {
        Some(catalog) => gateway.with_session_catalog(catalog),
        None => gateway,
    };
    let gateway = match backing.changes.clone() {
        Some((changes, digests)) => gateway.with_changes(changes, digests),
        None => gateway,
    };
    let gateway = match backing.model_calls.clone() {
        Some(model_calls) => gateway.with_model_calls(model_calls),
        None => gateway,
    };
    let gateway = match backing.tenancy.clone() {
        Some(store) => gateway.with_tenancy_store(store),
        None => gateway,
    };
    match backing.pages.clone() {
        Some(pages) => gateway.with_topic_pages(pages),
        None => gateway,
    }
}

pub(crate) fn trusted_proxies(config: &Config) -> Vec<config::model::TrustedProxy> {
    config
        .gateway
        .as_ref()
        .map(|section| section.trusted_proxies.clone())
        .unwrap_or_default()
}

async fn drop_box(
    gateway: Gateway,
    config: &Config,
    tracker: &tokio_util::task::TaskTracker,
    shutdown: &CancellationToken,
) -> Result<Gateway, ComposeError> {
    let enabled = config
        .gateway
        .as_ref()
        .map(|section| &section.drops)
        .filter(|drops| drops.enabled);
    let Some(section) = enabled else {
        return Ok(gateway);
    };
    let root = section
        .root
        .clone()
        .unwrap_or_else(|| gateway.settings().state_dir.join("drops"));
    let drops_config = gateway::drops::DropsConfig::new(root, section);
    let drops =
        tokio::task::spawn_blocking(move || gateway::drops::Drops::open(&drops_config)).await??;
    let drops = Arc::new(drops);
    drops.spawn_sweeper(tracker, shutdown.child_token());
    Ok(gateway.with_drops(drops))
}

fn with_session_stores(gateway: Gateway, env: &Environment) -> Gateway {
    let roots = store_roots(env);
    let gateway = gateway
        .with_transcript_reader(Arc::new(capture::listing::StoreTranscriptReader::new(
            roots.clone(),
        )))
        .with_session_lister(Arc::new(capture::listing::StoreSessionLister::new(roots)));
    match harness_setup(env) {
        Ok(setup) => gateway.with_harness_setup(Arc::new(setup)),
        Err(error) => {
            tracing::warn!(%error, "harness setup is off: the node cannot name its own binary or state directory");
            gateway
        }
    }
}

fn harness_setup(
    env: &Environment,
) -> Result<capture::harnesses::HarnessSetupService, ComposeError> {
    let backups = config::Paths::new(env)?.hub_home().join("backups");
    let executable = std::env::current_exe()?;
    Ok(capture::harnesses::HarnessSetupService::new(
        env, executable, backups,
    ))
}

async fn session_registry(
    owners: std::path::PathBuf,
    node_owner: Option<gateway::tenancy::UserId>,
) -> Result<SessionRegistry, ComposeError> {
    let registry =
        tokio::task::spawn_blocking(move || SessionRegistry::default().with_owners_file(owners))
            .await?;
    Ok(match node_owner {
        Some(owner) => registry.with_node_owner(owner),
        None => registry,
    })
}

impl Composition {
    pub async fn validate_users(config: &Config, env: &Environment) -> Result<(), ComposeError> {
        identities::load(config, env).await?;
        Ok(())
    }

    pub async fn failed(&mut self) -> Result<(), ComposeError> {
        tokio::select! {
            result = self.tasks.join_next(), if !self.tasks.is_empty() => {
                if let Some(result) = result { result??; }
                if self.cancel.is_cancelled() {
                    return std::future::pending().await;
                }
                Err(ComposeError::Invalid("gateway task stopped unexpectedly".into()))
            }
            else => std::future::pending().await,
        }
    }
    pub async fn build(
        config: &Config,
        env: &Environment,
        store: Arc<dyn MemoryStore>,
        shutdown: &CancellationToken,
        activities: &kernel::shutdown::Activities,
    ) -> Result<Self, ComposeError> {
        let factory = crate::models::ModelFactory::from_config(config, env, shutdown)
            .await?
            .with_activities(activities.clone());
        Self::with_models(config, env, store, shutdown, &factory).await
    }

    pub async fn on_postgres(
        config: &Config,
        env: &Environment,
        memory: &Arc<PostgresMemory>,
        shutdown: &CancellationToken,
        activities: &kernel::shutdown::Activities,
    ) -> Result<Self, ComposeError> {
        let url = config.memory_postgres_url(env).ok_or_else(|| {
            ComposeError::Invalid("no Postgres URL for the memory database".into())
        })?;
        let backing = Backing {
            export: Some(Arc::new(MemoryExporter::postgres(Arc::clone(memory), url))),
            topics: Some((memory.clone(), memory.clone())),
            decisions: Some(memory.clone()),
            facts: Some(memory.clone()),
            review: Some(memory.clone()),
            pages: Some(memory.clone()),
            tidy: Some((memory.clone(), memory.clone())),
            flagged: Some(memory.clone()),
            deletions: Some(memory.clone()),
            catalog: Some(memory.clone()),
            changes: Some((memory.clone(), memory.clone())),
            model_calls: Some(memory.clone()),
            summaries: Some(memory.clone()),
            tenancy: Some(Arc::new(memory_postgres::PostgresTenancy::new(
                memory.pool().clone(),
            ))),
            sessions: Some(SessionStore::Postgres(Arc::clone(memory))),
        };
        let factory = crate::models::ModelFactory::from_config(config, env, shutdown)
            .await?
            .with_activities(activities.clone());
        let store: Arc<dyn MemoryStore> = memory.clone();
        let summaries = memory.spawn_summary_watch(shutdown.child_token()).await?;
        let mut composition =
            Self::compose(config, env, store, backing, shutdown, &factory).await?;
        composition
            .tasks
            .spawn(async move { summaries.await.map_err(ComposeError::Task) });
        Ok(composition)
    }

    pub(crate) async fn on_sqlite(
        config: &Config,
        env: &Environment,
        store: &Arc<SqliteStore>,
        shutdown: &CancellationToken,
        activities: &kernel::shutdown::Activities,
    ) -> Result<Self, ComposeError> {
        let backing = Backing {
            export: Some(Arc::new(MemoryExporter::sqlite(Arc::clone(store)))),
            topics: Some((store.clone(), store.clone())),
            decisions: Some(store.clone()),
            facts: Some(store.clone()),
            review: Some(store.clone()),
            pages: Some(store.clone()),
            tidy: Some((store.clone(), store.clone())),
            flagged: Some(store.clone()),
            deletions: Some(store.clone()),
            catalog: Some(store.clone()),
            changes: Some((store.clone(), store.clone())),
            model_calls: Some(store.clone()),
            summaries: Some(store.clone()),
            tenancy: Some(store.clone()),
            sessions: Some(SessionStore::Sqlite(SqliteStore::clone(store))),
        };
        let factory = crate::models::ModelFactory::from_config(config, env, shutdown)
            .await?
            .with_activities(activities.clone());
        let memory: Arc<dyn MemoryStore> = store.clone();
        Self::compose(config, env, memory, backing, shutdown, &factory).await
    }

    pub async fn with_models(
        config: &Config,
        env: &Environment,
        store: Arc<dyn MemoryStore>,
        shutdown: &CancellationToken,
        factory: &crate::models::ModelFactory,
    ) -> Result<Self, ComposeError> {
        Self::compose(config, env, store, Backing::default(), shutdown, factory).await
    }

    async fn compose(
        config: &Config,
        env: &Environment,
        store: Arc<dyn MemoryStore>,
        mut backing: Backing,
        shutdown: &CancellationToken,
        factory: &crate::models::ModelFactory,
    ) -> Result<Self, ComposeError> {
        let memory = Arc::clone(&store);
        let settings = gateway_settings(config, env)?;
        let node_name = settings.node.clone();
        let receiver_node = settings.node.clone();
        let section = config.gateway.clone();
        let tls_env = env.clone();
        let shared = config::shared_dir(env);
        let local_owner = local_owner(&settings.state_dir).await?;
        let bind = tokio::task::spawn_blocking(move || {
            gateway::server::BindSource::resolve_section(
                section.as_ref(),
                &tls_env,
                &node_name,
                &shared,
            )
        })
        .await?;
        let identity = identities::load(config, env).await?;
        let owner_scope = identity.owner_scope();
        let sessions = Arc::new(
            session_registry(
                gateway::sessions::owners_file(&settings.state_dir),
                identity.tenancy.local_user().cloned(),
            )
            .await?
            .with_shutdown(shutdown.child_token()),
        );
        let mut gateway = models::attach(Gateway::new(store, settings), config, env, factory)?
            .with_tenancy(identity.tenancy.clone())
            .with_local_owner(local_owner)
            .with_auth_policy(gateway::AuthPolicy::from_config(config))
            .with_trusted_proxies(trusted_proxies(config))
            .with_sessions(sessions);
        gateway = with_backing(with_session_stores(gateway, env), &backing);
        let tracker = tokio_util::task::TaskTracker::new();
        gateway = drop_box(gateway, config, &tracker, shutdown).await?;
        gateway = account::attach(gateway, config, env, &bind, shutdown, &tracker).await?;
        gateway = gateway.with_receive_desk(Arc::new(NodeReceiver::new(
            config,
            env,
            identity.primary(),
            receiver_node,
            Arc::clone(&memory),
        )));
        if let Some(sessions) = backing.sessions.take() {
            let desk = Arc::new(NodeDesk::new::<ComposeError>(
                config,
                env,
                identity.primary(),
                sessions,
                memory,
            )?);
            tracker.spawn(Arc::clone(&desk).sweep_until(shutdown.child_token()));
            gateway = gateway.with_share_desk(desk);
        }
        identities::keep_spaces(&tracker, backing.tenancy.clone(), &identity, shutdown);
        spool::replay_captures(&tracker, env, shutdown);
        let started = plugins::started(gateway, env, vec![bind.port]);
        let gateway = Box::pin(owner_scope.clone().run(started)).await?;
        Ok(Self {
            gateway,
            tasks: JoinSet::new(),
            background: tracker,
            model_tasks: factory.tasks(),
            cancel: shutdown.clone(),
        })
    }

    #[must_use]
    pub fn gateway(&self) -> &Arc<Gateway> {
        &self.gateway
    }

    pub fn router(&self, secure: bool) -> axum::Router {
        self.gateway.router(secure)
    }

    pub async fn close(mut self) -> Result<(), ComposeError> {
        self.cancel.cancel();
        self.gateway.close().await;
        while let Some(result) = self.tasks.join_next().await {
            result??;
        }
        self.background.close();
        self.background.wait().await;
        self.model_tasks.close().await;
        Ok(())
    }
}
