mod import;

use std::path::PathBuf;
use std::sync::Arc;

use crate::worker::ComposedRunner;
use config::{Config, Environment, ServiceSwitch};
use gateway::server::ServeOptions;
use kernel::filing::FilingStore;
use kernel::memory::jobs::UnembeddedStore;
use kernel::memory::{EmbeddingStore, MemoryStore, SessionDeletion, SummaryStore};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue, MAINTENANCE_PRIORITY};
use kernel::shutdown::Activities;
use kernel::tidy::TidyStore;
use kernel::topics::DecisionStore;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::wiki::builder::WikiBuilderStore;
use kernel::wiki::consolidate::WikiConsolidateStore;
use memory_sqlite::{QueryEmbed, SqliteStore};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use workers::ProcessRole;

use crate::Exit;
use crate::service::{StartupError, listen, report};
use crate::sidecar::{self, SidecarError};
use crate::worker::{self, RootError, WorkerPorts};

#[derive(Debug, Error)]
enum LocalError {
    #[error(transparent)]
    Import(#[from] import::Error),
    #[error(transparent)]
    Startup(#[from] StartupError),
    #[error(transparent)]
    Worker(#[from] RootError),
    #[error(transparent)]
    Mcp(#[from] SidecarError),
    #[error(transparent)]
    Store(#[from] memory_sqlite::SqliteError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Queue(#[from] kernel::queue::JobQueueError),
}

impl LocalError {
    fn exit(&self) -> Exit {
        match self {
            Self::Startup(error) => error.exit(),
            Self::Worker(error) => error.exit(),
            Self::Mcp(error) => error.exit(),
            Self::Import(_) | Self::Store(_) | Self::Join(_) | Self::Queue(_) => Exit::Failure,
        }
    }
}

pub async fn start(env: &Environment, prepared: cli::Prepared) -> Exit {
    let cli::Prepared {
        config,
        claim,
        database,
    } = prepared;
    let mut held = HeldClaim { claim: Some(claim) };
    let signals = match crate::signals::install() {
        Ok(signals) => signals,
        Err(exit) => return exit,
    };
    let activities = Activities::new();
    let token = signals.token();
    let run_token = token.clone();
    let result = crate::shutdown::bounded(
        &token,
        &activities,
        Box::pin(run(env, &config, database, run_token, activities.clone())),
    )
    .await;
    let finished = signals.finish().await;
    let released = held.release();
    match result {
        Ok(inner) => finish(inner, finished, released),
        Err(running) => {
            tracing::debug!("bounded shutdown: {running}");
            if let Err(error) = released {
                tracing::error!("pid file removal failed: {error}");
            }
            match finished {
                Ok(()) => Exit::Failure,
                Err(exit) => exit,
            }
        }
    }
}

fn finish(
    result: Result<(), LocalError>,
    finished: Result<(), Exit>,
    released: Result<(), config::StateError>,
) -> Exit {
    match (result, finished, released) {
        (Ok(()), Ok(()), Ok(())) => Exit::Ok,
        (Err(error), _, released) => {
            if let Err(release) = released {
                tracing::error!("pid file removal failed: {release}");
            }
            report_local(&error);
            error.exit()
        }
        (Ok(()), Err(exit), released) => {
            if let Err(release) = released {
                tracing::error!("pid file removal failed: {release}");
            }
            exit
        }
        (Ok(()), Ok(()), Err(error)) => {
            tracing::error!("pid file removal failed: {error}");
            Exit::Failure
        }
    }
}

fn report_local(error: &LocalError) {
    match error {
        LocalError::Startup(error) => report(error),
        LocalError::Worker(error) => worker::report_root(error),
        LocalError::Mcp(error) => sidecar::report_error(error),
        LocalError::Import(_)
        | LocalError::Store(_)
        | LocalError::Join(_)
        | LocalError::Queue(_) => {
            tracing::error!("{error}");
        }
    }
}

struct GatewayNode {
    composition: crate::composition::Composition,
    listener: tokio::net::TcpListener,
    tls: Option<gateway::tls::TlsPaths>,
    runners: Vec<(ProcessRole, ComposedRunner<SqliteStore>)>,
    activities: Activities,
}

async fn run(
    env: &Environment,
    config: &Config,
    database: PathBuf,
    shutdown: CancellationToken,
    activities: Activities,
) -> Result<(), LocalError> {
    let _guard = shutdown.clone().drop_guard();
    let embed = crate::service::sqlite_query_embed(config, env, &shutdown, &activities)?;
    let model = crate::service::sqlite_vector_model(config, env);
    let store = open_store(database, embed, model).await?;
    import::pending(&store, config, &shutdown).await?;
    let switches = config.service_switches();
    let node = bind_node(&store, &switches, config, env, &shutdown, &activities).await?;
    supervise(node, &switches, config, env, store, shutdown).await
}

async fn bind_node(
    store: &Arc<SqliteStore>,
    switches: &[ServiceSwitch],
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<GatewayNode, LocalError> {
    let runners = build_runners(
        store,
        worker_roles(switches),
        config,
        env,
        shutdown,
        activities,
    )
    .await?;
    let (listener, tls) = listen(config, env).await?;
    let composition =
        crate::composition::Composition::on_sqlite(config, env, store, shutdown, activities)
            .await
            .map_err(StartupError::from)?;
    Ok(GatewayNode {
        composition,
        listener,
        tls,
        runners,
        activities: activities.clone(),
    })
}

async fn supervise(
    node: GatewayNode,
    switches: &[ServiceSwitch],
    config: &Config,
    env: &Environment,
    store: Arc<SqliteStore>,
    shutdown: CancellationToken,
) -> Result<(), LocalError> {
    let GatewayNode {
        composition,
        listener,
        tls,
        runners,
        activities,
    } = node;
    let addr = listener.local_addr().map_err(StartupError::from)?;
    let secure = tls.is_some();
    tracing::info!(
        "gateway listening on {addr}{}",
        if secure { " (tls)" } else { " (plain)" }
    );
    let tracker = TaskTracker::new();
    let (failed_tx, mut failed_rx) = mpsc::unbounded_channel();
    if let Err(error) = (NodeBoot {
        tracker: &tracker,
        failed: &failed_tx,
        switches,
        config,
        env,
        store: &store,
        shutdown: &shutdown,
        plugins: composition.gateway().plugins().cloned(),
    })
    .start(runners)
    .await
    {
        shutdown.cancel();
        note_close(composition.close().await.map_err(StartupError::from));
        drop(failed_tx);
        tracker.close();
        tracker.wait().await;
        return Err(error);
    }
    let serving = gateway::server::serve_router_with(
        listener,
        composition.router(secure),
        tls,
        shutdown.clone(),
        ServeOptions::default(),
        activities.clone(),
    );
    tokio::pin!(serving);
    let outcome = await_node(composition, &mut failed_rx, &mut serving, &shutdown).await;
    drop(failed_tx);
    tracker.close();
    tracker.wait().await;
    outcome?;
    if let Ok(error) = failed_rx.try_recv() {
        return Err(error);
    }
    tracing::info!("gateway stopped");
    Ok(())
}

struct NodeBoot<'a> {
    tracker: &'a TaskTracker,
    failed: &'a mpsc::UnboundedSender<LocalError>,
    switches: &'a [ServiceSwitch],
    config: &'a Config,
    env: &'a Environment,
    store: &'a Arc<SqliteStore>,
    shutdown: &'a CancellationToken,
    plugins: Option<Arc<gateway::plugins::PluginHost>>,
}

impl NodeBoot<'_> {
    async fn start(
        &self,
        runners: Vec<(ProcessRole, ComposedRunner<SqliteStore>)>,
    ) -> Result<(), LocalError> {
        if self.switches.contains(&ServiceSwitch::Workers) {
            enqueue_sweep(
                self.store,
                FixedJob::EnqueueUnembedded,
                "unembedded-sweep",
                MAINTENANCE_PRIORITY,
            )
            .await?;
        }
        if self.switches.contains(&ServiceSwitch::Wiki) {
            enqueue_sweep(
                self.store,
                FixedJob::EnqueueWikiBackfill,
                "wiki-backfill",
                MAINTENANCE_PRIORITY,
            )
            .await?;
            enqueue_sweep(self.store, FixedJob::ConsolidateWiki, "wiki-consolidate", 5).await?;
        }
        spawn_workers(self.tracker, self.failed, self.shutdown, runners);
        self.spawn_mcp()
    }

    fn spawn_mcp(&self) -> Result<(), LocalError> {
        if !self.switches.contains(&ServiceSwitch::Mcp) {
            return Ok(());
        }
        let task = sidecar::memory_future(
            self.config,
            Arc::clone(self.store) as Arc<dyn MemoryStore>,
            self.plugins
                .clone()
                .map(|host| host as Arc<dyn kernel::plugins::PluginTools>),
            sidecar::display_zone(self.env),
            self.shutdown.clone(),
        )?;
        let failed = self.failed.clone();
        self.tracker.spawn(async move {
            if let Err(error) = task.await {
                deliver(&failed, LocalError::Mcp(error));
            }
        });
        Ok(())
    }
}

async fn enqueue_sweep(
    store: &SqliteStore,
    sweep: FixedJob,
    key: &str,
    priority: i16,
) -> Result<(), LocalError> {
    store
        .enqueue(Job {
            name: JobName::Fixed(sweep),
            payload: wire::JsonObject::default(),
            key: Some(key.to_owned()),
            key_mode: JobKeyMode::Replace,
            priority,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    Ok(())
}

async fn await_node(
    mut composition: crate::composition::Composition,
    failed_rx: &mut mpsc::UnboundedReceiver<LocalError>,
    serving: &mut std::pin::Pin<
        &mut impl std::future::Future<Output = Result<(), gateway::server::ServeError>>,
    >,
    shutdown: &CancellationToken,
) -> Result<(), LocalError> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => {
            composition.close().await.map_err(StartupError::from)?;
            serving.await.map_err(StartupError::from).map_err(LocalError::from)
        }
        Some(error) = failed_rx.recv() => {
            shutdown.cancel();
            note_close(composition.close().await.map_err(StartupError::from));
            note_serve(serving.await.map_err(StartupError::from));
            Err(error)
        }
        result = composition.failed() => {
            shutdown.cancel();
            composition.close().await.map_err(StartupError::from)?;
            result.map_err(StartupError::from)?;
            serving.await.map_err(StartupError::from).map_err(LocalError::from)
        }
        result = &mut *serving => {
            shutdown.cancel();
            composition.close().await.map_err(StartupError::from)?;
            result.map_err(StartupError::from).map_err(LocalError::from)
        }
    }
}

fn note_close(result: Result<(), StartupError>) {
    if let Err(error) = result {
        tracing::error!("composition close failed: {error}");
    }
}

fn note_serve(result: Result<(), StartupError>) {
    if let Err(error) = result {
        tracing::error!("gateway serve ended during shutdown: {error}");
    }
}

async fn open_store(
    path: PathBuf,
    embed: Option<QueryEmbed>,
    model: Option<String>,
) -> Result<Arc<SqliteStore>, LocalError> {
    let store = tokio::task::spawn_blocking(move || SqliteStore::open(path)).await??;
    let store = match embed {
        Some(embed) => {
            let store = store.with_query_embed(embed);
            store.arm_embed();
            store
        }
        None => store,
    };
    Ok(Arc::new(match model {
        Some(model) => store.with_embed_model(&model),
        None => store,
    }))
}

async fn build_runners(
    store: &Arc<SqliteStore>,
    roles: Vec<ProcessRole>,
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<Vec<(ProcessRole, ComposedRunner<SqliteStore>)>, LocalError> {
    let mut built = Vec::new();
    for role in roles {
        let ports = WorkerPorts {
            queue: Arc::clone(store),
            targets: Arc::clone(store) as Arc<dyn EmbeddingStore>,
            summaries: Arc::clone(store) as Arc<dyn SummaryStore>,
            wiki: Arc::clone(store) as Arc<dyn WikiBuilderStore>,
            entity: None,
            consolidate: Some(Arc::clone(store) as Arc<dyn WikiConsolidateStore>),
            recompile: None,
            topics: Arc::clone(store) as Arc<dyn TopicStore>,
            filing: Arc::clone(store) as Arc<dyn FilingStore>,
            pages: Arc::clone(store) as Arc<dyn TopicPageStore>,
            decisions: Arc::clone(store) as Arc<dyn DecisionStore>,
            changes: Arc::clone(store) as Arc<dyn kernel::changes::ChangeFeed>,
            digests: Arc::clone(store) as Arc<dyn kernel::changes::DigestStore>,
            tidy: Arc::clone(store) as Arc<dyn TidyStore>,
            lineage: worker::session_lineage(env),
            load: None,
            scopes: None,
        };
        let mut runner =
            worker::build_runner(role, ports, config, env, shutdown, activities).await?;
        if role == ProcessRole::Embedder {
            runner.runner = runner
                .runner
                .with_unembedded(Arc::clone(store) as Arc<dyn UnembeddedStore>);
        }
        runner.runner = runner
            .runner
            .with_session_deletion(Arc::clone(store) as Arc<dyn SessionDeletion>);
        if matches!(
            role,
            ProcessRole::Embedder | ProcessRole::Summarizer | ProcessRole::Categorizer
        ) {
            runner.runner = runner.runner.with_schedules(true).with_schedule_at_start();
        }
        built.push((role, runner));
    }
    Ok(built)
}

fn worker_roles(switches: &[ServiceSwitch]) -> Vec<ProcessRole> {
    let mut roles = Vec::new();
    for switch in switches {
        match switch {
            ServiceSwitch::Workers => {
                roles.push(ProcessRole::Embedder);
                roles.push(ProcessRole::Summarizer);
            }
            ServiceSwitch::Tagging => roles.push(ProcessRole::Categorizer),
            ServiceSwitch::Wiki => roles.push(ProcessRole::WikiBuilder),
            ServiceSwitch::Gateway | ServiceSwitch::Mcp => {}
        }
    }
    roles
}

fn spawn_workers(
    tracker: &TaskTracker,
    failed: &mpsc::UnboundedSender<LocalError>,
    shutdown: &CancellationToken,
    runners: Vec<(ProcessRole, ComposedRunner<SqliteStore>)>,
) {
    for (role, runner) in runners {
        let failed = failed.clone();
        let token = shutdown.clone();
        tracker.spawn(kernel::tenancy::Scope::carry(async move {
            tracing::info!("worker running {role}");
            if let Err(error) = runner.run(token).await {
                deliver(&failed, LocalError::Worker(RootError::Worker(error)));
            }
        }));
    }
}

fn deliver(failed: &mpsc::UnboundedSender<LocalError>, error: LocalError) {
    match failed.send(error) {
        Ok(()) => {}
        Err(send) => tracing::error!("component failure was not delivered: {}", send.0),
    }
}

struct HeldClaim {
    claim: Option<config::NodeClaim>,
}

impl HeldClaim {
    fn release(&mut self) -> Result<(), config::StateError> {
        match self.claim.take() {
            Some(claim) => claim.release(),
            None => Ok(()),
        }
    }
}

impl Drop for HeldClaim {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            match claim.release() {
                Ok(()) => {}
                Err(error) => tracing::error!("pid file removal failed: {error}"),
            }
        }
    }
}
