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
use kernel::wiki::entity_pass::EntityStore;
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
    let bindings = role_bindings(&config, &factory, &shutdown);
    factory.tasks().close().await;
    let bindings = bindings?;
    check_embeddings(role, &bindings)?;
    workers::tuned_embed(EmbedSettings::from_config(&config)?, env)?;
    workers::MemoryJobSettings::from_environment(env)?;
    workers::worker_concurrency(env)?;
    workers::tuned_summary(SummarySettings::standard(bindings.summarizer.model)?, env)?;
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
    crate::telemetry::arm(&config, env, crate::telemetry::worker_job(role));
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
        let entities: Arc<dyn EntityStore> = memory.clone();
        let consolidate: Arc<dyn WikiConsolidateStore> = memory.clone();
        let recompile: Arc<dyn WikiRecompileStore> = memory.clone();
        let topics: Arc<dyn TopicStore> = memory.clone();
        let filing: Arc<dyn FilingStore> = memory.clone();
        let pages: Arc<dyn TopicPageStore> = memory.clone();
        let decisions: Arc<dyn DecisionStore> = memory.clone();
        let changes: Arc<dyn kernel::changes::ChangeFeed> = memory.clone();
        let digests: Arc<dyn kernel::changes::DigestStore> = memory.clone();
        let tidy: Arc<dyn TidyStore> = memory.clone();
        let mut runner = build_runner(
            role,
            WorkerPorts {
                queue: Arc::new(queue),
                targets,
                summaries,
                wiki,
                entity: Some(entities),
                consolidate: Some(consolidate),
                recompile: Some(recompile),
                topics,
                filing,
                pages,
                decisions,
                changes,
                digests,
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
    pub entity: Option<Arc<dyn EntityStore>>,
    pub consolidate: Option<Arc<dyn WikiConsolidateStore>>,
    pub recompile: Option<Arc<dyn WikiRecompileStore>>,
    pub topics: Arc<dyn TopicStore>,
    pub filing: Arc<dyn FilingStore>,
    pub pages: Arc<dyn TopicPageStore>,
    pub decisions: Arc<dyn DecisionStore>,
    pub changes: Arc<dyn kernel::changes::ChangeFeed>,
    pub digests: Arc<dyn kernel::changes::DigestStore>,
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
    let bindings = role_bindings(config, factory, &cancel)?;
    check_embeddings(role, &bindings)?;
    let RoleBindings {
        summarizer,
        categorizer,
        wiki_writer,
        embedder,
        ..
    } = bindings;
    let entity_provider = Arc::clone(&wiki_writer.provider);
    let entity_model = wiki_writer.model.clone();
    let deps = WorkerDeps {
        targets: ports.targets,
        summaries: ports.summaries,
        summary: SummarySettings::standard(summarizer.model)?,
        summarizer: summarizer.provider,
        categorizer: workers::RoleModel {
            provider: categorizer.provider,
            model: categorizer.model,
        },
        embedder,
        embed: EmbedSettings::from_config(config)?,
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
        provider: wiki_writer.provider,
        model: wiki_writer.model,
        load: ports.load,
    });
    let runner = if let Some(store) = ports.entity {
        runner.with_entity_builder(workers::entity_builder::EntityBuilderDeps {
            store,
            provider: entity_provider,
            model: entity_model,
            settings: workers::entity_builder::MatcherSettings::from_config(config)
                .map_err(RootError::Invalid)?,
        })
    } else {
        runner
    };
    let runner = runner
        .with_topic_pages(Arc::clone(&ports.topics), ports.pages, ports.decisions)
        .with_tidy(ports.tidy)
        .with_daily_digests(ports.changes, ports.digests)
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

#[expect(
    missing_debug_implementations,
    reason = "role bindings hold kernel model ports without Debug"
)]
pub struct RoleBindings {
    pub summarizer: crate::models::ModelBinding,
    pub categorizer: crate::models::ModelBinding,
    pub wiki_writer: crate::models::ModelBinding,
    pub embedder: Arc<dyn kernel::model::ModelProvider>,
    pub embeddings: bool,
}

fn check_embeddings(role: ProcessRole, bindings: &RoleBindings) -> Result<(), RootError> {
    if matches!(role, ProcessRole::Embedder | ProcessRole::All) && !bindings.embeddings {
        return Err(
            crate::models::ModelError::Cli(providers::cli::CliError::MissingEmbedder).into(),
        );
    }
    Ok(())
}

fn resolved_models(
    config: &Config,
    env: &Environment,
) -> Result<config::ResolvedModels, RootError> {
    config
        .memory_models(env)
        .map_err(|error| RootError::Invalid(error.to_string()))
}

async fn model_factory(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<crate::models::ModelFactory, RootError> {
    let models = resolved_models(config, env)?;
    let ids = [
        &models.default,
        &models.summarizer.choice,
        &models.categorizer.choice,
        &models.wiki_writer.choice,
    ]
    .into_iter()
    .map(|choice| choice.provider.as_str())
    .collect();
    Ok(crate::models::ModelFactory::for_providers(config, env, ids, shutdown).await?)
}

pub fn role_bindings(
    config: &Config,
    factory: &crate::models::ModelFactory,
    shutdown: &CancellationToken,
) -> Result<RoleBindings, RootError> {
    let models = factory
        .resolve_models(config)
        .map_err(|error| RootError::Invalid(error.to_string()))?;
    let (embedder, embeddings) = match &models.embedder.choice {
        Some(choice) if !models.embedder.inherits_default => (
            crate::models::ModelFactory::embedding_provider(config, choice)?,
            true,
        ),
        Some(_) | None => {
            let default = factory.build_choice(config, &models.default, shutdown)?;
            (default.provider, default.embeddings)
        }
    };
    Ok(RoleBindings {
        summarizer: factory.build_choice(config, &models.summarizer.choice, shutdown)?,
        categorizer: factory.build_choice(config, &models.categorizer.choice, shutdown)?,
        wiki_writer: factory.build_choice(config, &models.wiki_writer.choice, shutdown)?,
        embedder,
        embeddings,
    })
}
