use std::sync::Arc;

use config::{Config, Environment};
use kernel::filing::FilingStore;
use kernel::memory::{EmbeddingStore, MemoryStore, SummaryStore};
use kernel::queue::JobQueue;
use kernel::shutdown::Activities;
use kernel::tidy::TidyStore;
use kernel::topics::DecisionStore;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::transcript::SessionLister;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::recompile::WikiRecompileStore;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use workers::{
    EMBED_CONCURRENCY, EmbedSettings, ProcessRole, Runner, RunnerConfig, SummarySettings,
    WorkerDeps,
};

use crate::Exit;
use crate::service::{StartupError, load, postgres, postgres_url, report};
use memory_postgres::Pooling;

#[derive(Debug, Error)]
pub enum RootError {
    #[error(transparent)]
    Startup(#[from] StartupError),
    #[error(transparent)]
    Worker(#[from] workers::WorkerError),
    #[error(transparent)]
    Provider(#[from] crate::models::ModelError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Invalid(String),
}

impl RootError {
    pub(crate) fn exit(&self) -> Exit {
        match self {
            Self::Startup(error) => error.exit(),
            Self::Worker(_) | Self::Provider(_) | Self::Join(_) | Self::Invalid(_) => Exit::Failure,
        }
    }
}

const WIKI_EXTRACTION: &str = "RIVETHUB_WIKI_EXTRACTION";

fn wiki_extraction(env: &Environment) -> bool {
    env.text(WIKI_EXTRACTION) == Some("1")
}

fn require_wiki_extraction(env: &Environment, role: ProcessRole) -> Result<(), RootError> {
    if role
        .claim_roles()?
        .contains(&kernel::queue::WorkerRole::WikiBuilder)
        && !wiki_extraction(env)
    {
        return Err(RootError::Invalid(format!(
            "worker role {role} claims extract-wiki jobs and would complete them without extracting; set {WIKI_EXTRACTION}=1"
        )));
    }
    Ok(())
}

pub async fn start(env: &Environment, role: ProcessRole, check: bool) -> Exit {
    if check {
        return finish(check_role(env, role).await, "worker configuration valid");
    }
    let env = env.clone();
    crate::shutdown::signals_then(
        async move |shutdown, activities| run(&env, role, shutdown, &activities).await,
        |result| finish(result, ""),
    )
    .await
}

fn finish(result: Result<(), RootError>, ready: &str) -> Exit {
    match result {
        Ok(()) => {
            if !ready.is_empty() {
                tracing::info!("{ready}");
            }
            Exit::Ok
        }
        Err(error) => {
            report_root(&error);
            error.exit()
        }
    }
}

pub(crate) fn report_root(error: &RootError) {
    match error {
        RootError::Startup(error) => report(error),
        RootError::Worker(_)
        | RootError::Provider(_)
        | RootError::Join(_)
        | RootError::Invalid(_) => tracing::error!("{error}"),
    }
}

async fn check_role(env: &Environment, role: ProcessRole) -> Result<(), RootError> {
    require_wiki_extraction(env, role)?;
    let owned = env.clone();
    let config = tokio::task::spawn_blocking(move || load(&owned)).await??;
    postgres_url(&config, env)?;
    let shutdown = CancellationToken::new();
    let factory = model_factory(&config, env, &shutdown).await?;
    let binding = model_binding(&config, &factory, &shutdown);
    factory.tasks().close().await;
    let binding = binding?;
    check_embeddings(role, &binding)?;
    let model = binding.model;
    workers::tuned_embed(EmbedSettings::from_config(&config)?, env)?;
    workers::MemoryJobSettings::from_environment(env)?;
    workers::worker_concurrency(env)?;
    workers::tuned_summary(SummarySettings::standard(model)?, env)?;
    crate::identity::announce_resolved("worker", &config, env).map_err(StartupError::from)?;
    Ok(())
}

async fn run(
    env: &Environment,
    role: ProcessRole,
    shutdown: CancellationToken,
    activities: &Activities,
) -> Result<(), RootError> {
    let _guard = shutdown.clone().drop_guard();
    require_wiki_extraction(env, role)?;
    let owned = env.clone();
    let config = tokio::task::spawn_blocking(move || load(&owned)).await??;
    crate::identity::announce_resolved("worker", &config, env).map_err(StartupError::from)?;
    let memory = Arc::new(postgres(&config, env, &shutdown, activities, Pooling::OnDemand).await?);
    let result = async {
        let settings = workers::MemoryJobSettings::from_environment(env)?;
        let queue = memory
            .queue()
            .await
            .map_err(StartupError::from)?
            .flagging_after(settings.flag_after)
            .claiming_at_most(settings.claim_ceiling.or_else(|| {
                (!settings.backfill_enabled).then_some(kernel::wiki::builder::BACKFILL_PRIORITY - 1)
            }));
        let targets: Arc<dyn EmbeddingStore> = memory.clone();
        let summaries: Arc<dyn SummaryStore> = memory.clone();
        let wiki: Arc<dyn WikiBuilderStore> = memory.clone();
        let consolidate: Arc<dyn WikiConsolidateStore> = memory.clone();
        let recompile: Arc<dyn WikiRecompileStore> = memory.clone();
        let topics: Arc<dyn TopicStore> = memory.clone();
        let filing: Arc<dyn FilingStore> = memory.clone();
        let pages: Arc<dyn TopicPageStore> = memory.clone();
        let decisions: Arc<dyn DecisionStore> = memory.clone();
        let facts: Arc<dyn kernel::facts::FactStore> = memory.clone();
        let tidy: Arc<dyn TidyStore> = memory.clone();
        let mut runner = build_runner(
            role,
            WorkerPorts {
                queue: Arc::new(queue),
                targets,
                summaries,
                wiki,
                consolidate: Some(consolidate),
                recompile: Some(recompile),
                topics,
                filing,
                pages,
                decisions,
                facts,
                tidy,
                lineage: session_lineage(env),
                load: model_load(env)?,
                scopes: Some(owner_scopes(&memory, &config, env).await?),
            },
            &config,
            env,
            &shutdown,
            activities,
        )
        .await?;
        runner.runner = runner
            .runner
            .with_memory_jobs(memory.clone())
            .with_unembedded(memory.clone())
            .with_session_deletion(memory.clone())
            .with_job_settings(settings)
            .with_schedules(wiki_extraction(env));
        runner.runner = runner.runner.tuned(env)?;
        if let Some(concurrency) = workers::worker_concurrency(env)? {
            runner.runner = runner.runner.with_concurrency(concurrency);
        }
        tracing::info!("worker running {role}");
        drive(runner, shutdown.clone()).await
    }
    .await;
    shutdown.cancel();
    let closed = memory.close().await.map_err(StartupError::from);
    closed?;
    result?;
    tracing::info!("worker stopped");
    Ok(())
}

fn model_load(
    env: &Environment,
) -> Result<Option<Arc<dyn kernel::model::ModelLoadProbe>>, RootError> {
    Ok(crate::model_load::VllmMetrics::from_environment(env)
        .map_err(|error| RootError::Invalid(format!("model load probe: {error}")))?
        .map(|probe| Arc::new(probe) as Arc<dyn kernel::model::ModelLoadProbe>))
}

async fn owner_scopes(
    memory: &memory_postgres::PostgresMemory,
    config: &Config,
    env: &Environment,
) -> Result<workers::OwnerScopes, RootError> {
    Ok(workers::OwnerScopes {
        store: Arc::new(memory_postgres::PostgresTenancy::new(memory.pool().clone())),
        primary: crate::composition::identities::load(config, env)
            .await
            .map_err(StartupError::from)?
            .primary(),
    })
}

#[expect(
    missing_debug_implementations,
    reason = "worker dependencies are kernel ports without Debug"
)]
pub struct WorkerPorts<Q> {
    pub queue: Arc<Q>,
    pub targets: Arc<dyn EmbeddingStore>,
    pub summaries: Arc<dyn SummaryStore>,
    pub wiki: Arc<dyn WikiBuilderStore>,
    pub consolidate: Option<Arc<dyn WikiConsolidateStore>>,
    pub recompile: Option<Arc<dyn WikiRecompileStore>>,
    pub topics: Arc<dyn TopicStore>,
    pub filing: Arc<dyn FilingStore>,
    pub pages: Arc<dyn TopicPageStore>,
    pub decisions: Arc<dyn DecisionStore>,
    pub facts: Arc<dyn kernel::facts::FactStore>,
    pub tidy: Arc<dyn TidyStore>,
    pub lineage: Arc<dyn SessionLister>,
    pub load: Option<Arc<dyn kernel::model::ModelLoadProbe>>,
    pub scopes: Option<workers::OwnerScopes>,
}

pub(crate) fn session_lineage(env: &Environment) -> Arc<dyn SessionLister> {
    Arc::new(capture::listing::StoreSessionLister::new(
        crate::composition::store_roots(env),
    ))
}

pub async fn build_runner<Q: JobQueue + 'static>(
    role: ProcessRole,
    ports: WorkerPorts<Q>,
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<ComposedRunner<Q>, RootError> {
    let factory = model_factory(config, env, shutdown)
        .await?
        .with_activities(activities.clone());
    with_models(role, ports, config, &factory, shutdown, activities)
}

pub fn with_models<Q: JobQueue + 'static>(
    role: ProcessRole,
    ports: WorkerPorts<Q>,
    config: &Config,
    factory: &crate::models::ModelFactory,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<ComposedRunner<Q>, RootError> {
    role.claim_roles()?;
    let cancel = shutdown.child_token();
    let binding = model_binding(config, factory, &cancel)?;
    check_embeddings(role, &binding)?;
    let crate::models::ModelBinding {
        provider, model, ..
    } = binding;
    let deps = WorkerDeps {
        targets: ports.targets,
        summaries: ports.summaries,
        provider,
        embed: EmbedSettings::from_config(config)?,
        summary: SummarySettings::standard(model.clone())?,
        scopes: ports.scopes,
    };
    let runner = Runner::new(
        ports.queue,
        RunnerConfig::new(role, EMBED_CONCURRENCY)?,
        deps,
    )
    .with_wiki_builder(workers::wiki_builder::WikiBuilderDeps {
        store: ports.wiki,
        consolidate: ports.consolidate,
        recompile: ports.recompile,
        model,
        load: ports.load,
    })
    .with_topic_pages(
        Arc::clone(&ports.topics),
        ports.pages,
        ports.decisions,
        ports.facts,
    )
    .with_tidy(ports.tidy)
    .with_session_topics(workers::SessionTopics {
        store: ports.topics,
        filing: ports.filing,
        lineage: ports.lineage,
    })
    .with_activities(activities.clone());
    Ok(ComposedRunner {
        runner,
        tasks: factory.tasks(),
        cancel,
    })
}

#[expect(
    missing_debug_implementations,
    reason = "worker runner holds kernel ports without Debug"
)]
pub struct ComposedRunner<Q> {
    pub runner: Runner<Q>,
    tasks: crate::models::ModelTasks,
    cancel: CancellationToken,
}

impl<Q: JobQueue + 'static> ComposedRunner<Q> {
    pub async fn run(&self, shutdown: CancellationToken) -> Result<(), workers::WorkerError> {
        let result = self.runner.run(shutdown.clone()).await;
        self.cancel.cancel();
        self.tasks.close().await;
        result
    }
}

async fn drive<Q: JobQueue + 'static>(
    runner: ComposedRunner<Q>,
    shutdown: CancellationToken,
) -> Result<(), RootError> {
    let token = shutdown.child_token();
    let task = tokio::spawn(kernel::tenancy::Scope::carry(async move {
        runner.run(token).await
    }));
    let _abort = crate::shutdown::AbortOnDrop::arm(&task);
    tokio::pin!(task);
    let joined = tokio::select! {
        biased;
        () = shutdown.cancelled() => (&mut task).await,
        result = &mut task => result,
    };
    joined??;
    Ok(())
}

fn check_embeddings(
    role: ProcessRole,
    binding: &crate::models::ModelBinding,
) -> Result<(), RootError> {
    if matches!(role, ProcessRole::Embedder | ProcessRole::All) && !binding.embeddings {
        return Err(
            crate::models::ModelError::Cli(providers::cli::CliError::MissingEmbedder).into(),
        );
    }
    Ok(())
}

async fn model_factory(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<crate::models::ModelFactory, RootError> {
    Ok(
        crate::models::ModelFactory::for_provider(config, env, provider_id(config)?, shutdown)
            .await?,
    )
}

fn model_binding(
    config: &Config,
    factory: &crate::models::ModelFactory,
    shutdown: &CancellationToken,
) -> Result<crate::models::ModelBinding, RootError> {
    Ok(factory.build(
        config,
        provider_id(config)?,
        default_agent(config)?.model.as_deref(),
        shutdown,
    )?)
}

fn provider_id(config: &Config) -> Result<&str, RootError> {
    default_agent(config)?
        .provider
        .as_deref()
        .ok_or_else(|| RootError::Invalid("the default agent requires a provider".into()))
}

fn default_agent(config: &Config) -> Result<&config::model::AgentEntry, RootError> {
    let runtime = config
        .runtime
        .as_ref()
        .ok_or_else(|| RootError::Invalid("runtime is required for providers".into()))?;
    let name = runtime
        .default_agent
        .as_ref()
        .ok_or_else(|| RootError::Invalid("default_agent is required".into()))?;
    let agents = config
        .agents
        .as_ref()
        .ok_or_else(|| RootError::Invalid("agents are required for providers".into()))?;
    agents
        .get(name)
        .ok_or_else(|| RootError::Invalid(format!("default agent {name} is missing")))
}
