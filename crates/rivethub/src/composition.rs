mod devices;
pub(crate) mod identities;
mod mesh;
mod models;
pub(crate) mod plugins;
mod spool;
mod tasks;
mod terminals;

pub use capture::listing::store_roots;

use std::sync::Arc;
use std::time::Duration;

use cli::memory::MemoryExporter;
use config::{Config, Environment};
use gateway::Gateway;
use gateway::sessions::SessionRegistry;
use kernel::agent_presets::AgentPresetStore;
use kernel::filing::FilingStore;
use kernel::hub_state::HubStateStore;
use kernel::memory::MemoryExport;
use kernel::memory::MemoryStore;
use kernel::tidy::TidyStore;
use kernel::tidy::UpkeepStore;
use kernel::topics::DecisionStore;
use kernel::topics::StartupStore;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use memory_postgres::PostgresMemory;
use memory_sqlite::SqliteStore;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wire::gateway::hub_state::HubState;

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Node(#[from] crate::NodeError),
    #[error(transparent)]
    Voice(#[from] gateway::VoiceConfigError),
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
    Terminal(#[from] terminal::Error),
    #[error(transparent)]
    Provider(#[from] crate::models::ModelError),
    #[error(transparent)]
    Agent(#[from] agent_loop::Error),
    #[error(transparent)]
    Hook(#[from] agent_loop::hooks::HookError),
    #[error("gateway configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Mesh(#[from] gateway::mesh::RegistryError),
    #[error(transparent)]
    Channel(#[from] gateway::mesh::ChannelError),
    #[error(transparent)]
    Bind(#[from] gateway::server::BindError),
    #[error(transparent)]
    Devices(#[from] gateway::routes::devices::SetupError),
    #[error(transparent)]
    Pki(#[from] pki::Error),
    #[error(transparent)]
    Tasks(#[from] kernel::tasks::TaskError),
    #[error(transparent)]
    Plugins(#[from] gateway::plugins::HostError),
    #[error(transparent)]
    Memory(#[from] memory_postgres::Error),
}

pub async fn agent_presets(
    memory: &PostgresMemory,
) -> Result<Option<Arc<dyn AgentPresetStore>>, ComposeError> {
    Ok(memory
        .agent_presets()
        .await?
        .map(|store| Arc::new(store) as Arc<dyn AgentPresetStore>))
}

pub async fn hub_state(
    memory: &PostgresMemory,
) -> Result<Option<Arc<dyn HubStateStore<HubState>>>, ComposeError> {
    let store = memory.hub_state().await?;
    if store.is_none() {
        tracing::warn!(
            "rh_hub_state is missing on the memory database; hub state stays on this node until `rh db migrate` runs"
        );
    }
    Ok(store.map(|store| Arc::new(store) as Arc<dyn HubStateStore<HubState>>))
}

#[expect(
    missing_debug_implementations,
    reason = "Gateway contains kernel ports without Debug"
)]
pub struct Composition {
    gateway: Arc<Gateway>,
    audit_tasks: JoinSet<Result<(), agent_loop::hooks::HookError>>,
    audit_cancel: CancellationToken,
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
    presets: Option<Arc<dyn AgentPresetStore>>,
    hub_state: Option<Arc<dyn HubStateStore<HubState>>>,
    export: Option<Arc<dyn MemoryExport>>,
    topics: Option<(Arc<dyn TopicStore>, Arc<dyn FilingStore>)>,
    decisions: Option<Arc<dyn DecisionStore>>,
    facts: Option<Arc<dyn kernel::facts::FactStore>>,
    startup: Option<(Arc<dyn TopicPageStore>, Arc<dyn StartupStore>)>,
    tidy: Option<(Arc<dyn TidyStore>, Arc<dyn UpkeepStore>)>,
    flagged: Option<Arc<dyn kernel::flagged::FlaggedJobs>>,
    summaries: Option<Arc<dyn kernel::memory::SummaryWatch>>,
    deletions: Option<Arc<dyn kernel::memory::SessionDeletion>>,
    catalog: Option<Arc<dyn kernel::memory::SessionCatalog>>,
    changes: Option<(
        Arc<dyn kernel::changes::ChangeFeed>,
        Arc<dyn kernel::changes::DigestStore>,
    )>,
    tasks: Option<tasks::TaskPorts>,
    tenancy: Option<Arc<dyn kernel::tenancy::TenancyStore>>,
}

fn with_backing(gateway: Gateway, backing: &Backing) -> Gateway {
    let gateway = match backing.presets.clone() {
        Some(presets) => gateway.with_agent_presets(presets),
        None => gateway,
    };
    let gateway = match backing.hub_state.clone() {
        Some(store) => gateway.with_hub_state(store),
        None => gateway,
    };
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
    let gateway = match backing.tenancy.clone() {
        Some(store) => gateway.with_tenancy_store(store),
        None => gateway,
    };
    match backing.startup.clone() {
        Some((pages, kept)) => gateway.with_startup_memory(pages, kept),
        None => gateway,
    }
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
    let registry = tokio::task::spawn_blocking(move || {
        SessionRegistry::new(Duration::from_secs(300)).with_owners_file(owners)
    })
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
                Err(ComposeError::Invalid("mesh task stopped unexpectedly".into()))
            }
            result = self.audit_tasks.join_next(), if !self.audit_tasks.is_empty() => {
                if let Some(result) = result { result??; }
                if self.audit_cancel.is_cancelled() {
                    return std::future::pending().await;
                }
                Err(ComposeError::Invalid("audit task stopped unexpectedly".into()))
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
        let (ports, watching) = tasks::postgres(&url, shutdown).await?;
        let backing = Backing {
            presets: agent_presets(memory).await?,
            hub_state: hub_state(memory).await?,
            export: Some(Arc::new(MemoryExporter::postgres(Arc::clone(memory), url))),
            topics: Some((memory.clone(), memory.clone())),
            decisions: Some(memory.clone()),
            facts: Some(memory.clone()),
            startup: Some((memory.clone(), memory.clone())),
            tidy: Some((memory.clone(), memory.clone())),
            flagged: Some(memory.clone()),
            deletions: Some(memory.clone()),
            catalog: Some(memory.clone()),
            changes: Some((memory.clone(), memory.clone())),
            summaries: Some(memory.clone()),
            tasks: Some(ports),
            tenancy: Some(Arc::new(memory_postgres::PostgresTenancy::new(
                memory.pool().clone(),
            ))),
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
        composition.tasks.spawn(async move { Ok(watching.await??) });
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
            presets: None,
            hub_state: None,
            export: Some(Arc::new(MemoryExporter::sqlite(Arc::clone(store)))),
            topics: Some((store.clone(), store.clone())),
            decisions: Some(store.clone()),
            facts: Some(store.clone()),
            startup: Some((store.clone(), store.clone())),
            tidy: Some((store.clone(), store.clone())),
            flagged: Some(store.clone()),
            deletions: Some(store.clone()),
            catalog: Some(store.clone()),
            changes: Some((store.clone(), store.clone())),
            summaries: Some(store.clone()),
            tasks: Some(tasks::TaskPorts::of(store.as_ref())),
            tenancy: Some(store.clone()),
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
        backing: Backing,
        shutdown: &CancellationToken,
        factory: &crate::models::ModelFactory,
    ) -> Result<Self, ComposeError> {
        let settings = gateway_settings(config, env)?;
        let node_name = settings.node.clone();
        let section = config.gateway.clone();
        let tls_env = env.clone();
        let shared = config::shared_dir(env);
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
            .with_uploads(crate::service::uploads(env)?)
            .with_tenancy(identity.tenancy.clone())
            .with_identity_required()
            .with_client_certificate_required(bind.require_client)
            .with_sessions(sessions.clone());
        gateway = crate::service::with_voice(gateway, env)?;
        gateway = with_backing(with_session_stores(gateway, env), &backing);
        gateway = devices::attach(gateway, config, env, &identity.owner, shutdown).await?;
        let tracker = tokio_util::task::TaskTracker::new();
        identities::keep_spaces(&tracker, backing.tenancy.clone(), &identity, shutdown);
        spool::replay_captures(&tracker, env, shutdown);
        if config.memory_only() {
            gateway = gateway.with_memory_profile();
        }
        if config.terminal_enabled() && !config.memory_only() {
            let owner = identity.tenancy.local_user().cloned().ok_or_else(|| {
                ComposeError::Invalid("a local user is required for terminals".into())
            })?;
            gateway = gateway.with_terminal(
                terminals::build(config, env, sessions, &owner, shutdown, &tracker).await?,
            );
        }
        let audit_cancel = shutdown.child_token();
        let mut audit_tasks = JoinSet::new();
        let agents = if mesh::enabled(config) {
            models::build(config, env, factory, &audit_cancel, &mut audit_tasks).await?
        } else {
            std::collections::BTreeMap::new()
        };
        let mesh = mesh::start(config, env, agents, identity.tenancy, shutdown);
        let mut background = Box::pin(owner_scope.clone().run(mesh)).await?;
        let delegating = backing.tasks.is_some() && !config.memory_only();
        if !config.memory_only()
            && let Some(ports) = backing.tasks
        {
            let files_root = gateway.settings().files_root.clone();
            gateway = tasks::attach(gateway, ports, config, env, files_root).await?;
        }
        let started = plugins::started(gateway, env, plugins::node_ports(config, bind.port));
        let gateway = Box::pin(owner_scope.clone().run(started)).await?;
        if delegating && gateway.has_tasks() {
            owner_scope.enter(|| tasks::serve(&gateway, shutdown, &mut background));
        }
        Ok(Self {
            gateway,
            audit_tasks,
            audit_cancel,
            tasks: background,
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
        self.audit_cancel.cancel();
        while let Some(result) = self.audit_tasks.join_next().await {
            result??;
        }
        while let Some(result) = self.tasks.join_next().await {
            result??;
        }
        self.background.close();
        self.background.wait().await;
        self.model_tasks.close().await;
        Ok(())
    }
}
