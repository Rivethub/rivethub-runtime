mod delegation;
pub(crate) mod dial;

use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use config::model::TransportSection;
use config::{Config, Environment};
use kernel::memory::MemoryStore;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::Exit;
use crate::service::{StartupError, announce, load, postgres, report};
use memory_postgres::Pooling;

#[derive(Debug, Error)]
pub(crate) enum SidecarError {
    #[error(transparent)]
    Startup(#[from] StartupError),
    #[error(transparent)]
    Mcp(#[from] mcp::Error),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Dial(#[from] dial::DialError),
    #[error(transparent)]
    Compose(#[from] crate::composition::ComposeError),
    #[error(transparent)]
    Sqlite(#[from] memory_sqlite::SqliteError),
    #[error("memory.sqlite.path is not set; rh mcp reads the SQLite file rh local serves")]
    SqlitePath,
}

impl SidecarError {
    pub(crate) fn exit(&self) -> Exit {
        match self {
            Self::Startup(error) => error.exit(),
            Self::Mcp(error) => mcp_exit(error),
            Self::Join(_)
            | Self::Invalid(_)
            | Self::Compose(_)
            | Self::Dial(_)
            | Self::Sqlite(_)
            | Self::SqlitePath => Exit::Failure,
        }
    }
}

fn mcp_exit(error: &mcp::Error) -> Exit {
    match error {
        mcp::Error::Io(error) => io_exit(error),
        mcp::Error::SocketIo { source, .. } => io_exit(source),
        mcp::Error::SocketListening { .. }
        | mcp::Error::SocketNotSocket { .. }
        | mcp::Error::SocketChanged { .. }
        | mcp::Error::SocketProbeTimeout { .. } => Exit::Serve,
        mcp::Error::Arguments(_)
        | mcp::Error::Invalid(_)
        | mcp::Error::Refused(_)
        | mcp::Error::Cancelled
        | mcp::Error::Memory(_)
        | mcp::Error::WikiSlug(_)
        | mcp::Error::WikiMissing(_)
        | mcp::Error::WikiRedirect
        | mcp::Error::Task(_)
        | mcp::Error::Initialize(_)
        | mcp::Error::Service(_)
        | mcp::Error::Format(_) => Exit::Failure,
    }
}

fn io_exit(error: &std::io::Error) -> Exit {
    let kind = error.kind();
    if kind == std::io::ErrorKind::AddrInUse || kind == std::io::ErrorKind::AddrNotAvailable {
        Exit::Bind
    } else {
        Exit::Serve
    }
}

struct TcpEndpoint {
    host: String,
    port: u16,
    bearer: mcp::transport::Bearer,
}

struct UnixEndpoint {
    path: PathBuf,
    bearer: Option<mcp::transport::Bearer>,
}

enum Selected {
    Stdio,
    Network {
        tcp: Option<TcpEndpoint>,
        unix: Option<UnixEndpoint>,
    },
}

pub async fn start(env: &Environment, check: bool) -> Exit {
    if check {
        return finish(validate(env).await, "mcp configuration valid");
    }
    let env = env.clone();
    crate::shutdown::signals_then(
        async move |shutdown, activities| run(&env, shutdown, &activities).await,
        |result| finish(result, ""),
    )
    .await
}

fn finish(result: Result<(), SidecarError>, ready: &str) -> Exit {
    match result {
        Ok(()) => {
            if !ready.is_empty() {
                tracing::info!("{ready}");
            }
            Exit::Ok
        }
        Err(error) => {
            report_error(&error);
            error.exit()
        }
    }
}

pub(crate) fn report_error(error: &SidecarError) {
    match error {
        SidecarError::Startup(error) => report(error),
        SidecarError::Mcp(_)
        | SidecarError::Join(_)
        | SidecarError::Invalid(_)
        | SidecarError::Dial(_)
        | SidecarError::Compose(_)
        | SidecarError::Sqlite(_)
        | SidecarError::SqlitePath => {
            tracing::error!("{error}");
        }
    }
}

async fn validate(env: &Environment) -> Result<(), SidecarError> {
    let config = read_config(env).await?;
    let _selected = select(&config)?;
    crate::identity::announce_resolved("mcp", &config, env).map_err(StartupError::from)?;
    Ok(())
}

async fn run(
    env: &Environment,
    shutdown: CancellationToken,
    activities: &kernel::shutdown::Activities,
) -> Result<(), SidecarError> {
    let _guard = shutdown.clone().drop_guard();
    let config = read_config(env).await?;
    let selected = if config.sqlite_selected() {
        Selected::Stdio
    } else {
        select(&config)?
    };
    crate::identity::announce_resolved("mcp", &config, env).map_err(StartupError::from)?;
    let memory: Arc<dyn MemoryStore> = if config.sqlite_selected() {
        sqlite(&config, env, &shutdown, activities).await?
    } else {
        Arc::new(postgres(&config, env, &shutdown, activities, Pooling::OnDemand).await?)
    };
    let scope = crate::composition::identities::load(&config, env)
        .await?
        .owner_scope();
    let result = scope
        .run(run_server(
            &config,
            env,
            Arc::clone(&memory),
            selected,
            &shutdown,
        ))
        .await;
    shutdown.cancel();
    let closed = memory.close().await.map_err(StartupError::from);
    closed?;
    result?;
    tracing::info!("mcp stopped");
    Ok(())
}

async fn run_server(
    config: &Config,
    env: &Environment,
    memory: Arc<dyn MemoryStore>,
    selected: Selected,
    shutdown: &CancellationToken,
) -> Result<(), SidecarError> {
    let delegation: Arc<dyn kernel::tasks::Delegation> = Arc::new(
        delegation::GatewayDelegation::open(
            config,
            env,
            delegation::GatewayDelegation::session_parent(env),
        )
        .await?,
    );
    let folder = crate::composition::plugins::tools_folder(config, env)?;
    let plugins =
        mcp::plugins::SocketTools::new(folder.join("tools.sock"), folder.join("tools.token"))?;
    let server = mcp::Server::builder(memory, false, shutdown.child_token())
        .display_zone(display_zone(env))
        .delegation(delegation, surface(&selected))
        .plugins(Arc::new(plugins))
        .build()?;
    serve(server, selected, shutdown.clone()).await
}

pub(crate) async fn local_delegation(
    config: &Config,
    env: &Environment,
    gateway: url::Url,
) -> Result<Arc<dyn kernel::tasks::Delegation>, dial::DialError> {
    Ok(Arc::new(
        delegation::GatewayDelegation::local(config, env, gateway).await?,
    ))
}

pub(crate) fn memory_future(
    config: &Config,
    memory: Arc<dyn MemoryStore>,
    delegation: Option<Arc<dyn kernel::tasks::Delegation>>,
    plugins: Option<Arc<dyn kernel::plugins::PluginTools>>,
    zone: chrono_tz::Tz,
    shutdown: CancellationToken,
) -> Result<impl std::future::Future<Output = Result<(), SidecarError>> + Send + use<>, SidecarError>
{
    let selected = select(config)?;
    if matches!(selected, Selected::Stdio) {
        return Err(SidecarError::Invalid(
            "rh local serves MCP only when a host or socket is configured".into(),
        ));
    }
    let mut builder =
        mcp::Server::builder(memory, false, shutdown.child_token()).display_zone(zone);
    if let Some(port) = delegation {
        builder = builder.delegation(port, mcp::Surface::Stdio);
    }
    if let Some(port) = plugins {
        builder = builder.plugins(port);
    }
    let server = builder.build()?;
    Ok(async move {
        serve(server, selected, shutdown).await?;
        tracing::info!("mcp stopped");
        Ok(())
    })
}

fn surface(selected: &Selected) -> mcp::Surface {
    match selected {
        Selected::Stdio => mcp::Surface::Stdio,
        Selected::Network { .. } => mcp::Surface::Remote,
    }
}

fn load_standalone_or_node(env: &Environment) -> Result<Config, StartupError> {
    if let Some(location) = config::resolve_config_path(env)? {
        let standalone = Config::load(env, &location, config::Profile::Standalone)?;
        if standalone.config.sqlite_selected() {
            return Ok(announce(standalone));
        }
    }
    load(env)
}

async fn read_config(env: &Environment) -> Result<Config, SidecarError> {
    let owned = env.clone();
    Ok(tokio::task::spawn_blocking(move || load_standalone_or_node(&owned)).await??)
}

async fn sqlite(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &kernel::shutdown::Activities,
) -> Result<Arc<dyn MemoryStore>, SidecarError> {
    let path = config
        .memory
        .as_ref()
        .and_then(|memory| memory.sqlite.as_ref())
        .and_then(|sqlite| sqlite.path.clone())
        .ok_or(SidecarError::SqlitePath)?;
    let embed = crate::service::sqlite_query_embed(config, env, shutdown, activities)?;
    let model = crate::service::sqlite_vector_model(config, env);
    let store =
        tokio::task::spawn_blocking(move || memory_sqlite::SqliteStore::open(path)).await??;
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

fn select(config: &Config) -> Result<Selected, SidecarError> {
    let Some(section) = mcp_section(config) else {
        return Ok(Selected::Stdio);
    };
    if section.require_bearer_on_socket == Some(true) && section.auth_token.is_none() {
        return Err(SidecarError::Invalid(
            "mcp socket bearer is required".into(),
        ));
    }
    let tcp = tcp_endpoint(section)?;
    let unix = unix_endpoint(section)?;
    if tcp.is_none() && unix.is_none() {
        Ok(Selected::Stdio)
    } else {
        Ok(Selected::Network { tcp, unix })
    }
}

fn mcp_section(config: &Config) -> Option<&TransportSection> {
    config.transports.as_ref().and_then(|map| map.get("mcp"))
}

fn tcp_endpoint(section: &TransportSection) -> Result<Option<TcpEndpoint>, SidecarError> {
    let Some(host) = section.host.as_deref() else {
        return Ok(None);
    };
    if host.is_empty() {
        return Err(SidecarError::Invalid("mcp host is empty".into()));
    }
    if section.port == 0 {
        return Err(SidecarError::Invalid("mcp port must not be 0".into()));
    }
    let Some(token) = section.auth_token.as_deref() else {
        return Err(SidecarError::Invalid("mcp auth token is required".into()));
    };
    Ok(Some(TcpEndpoint {
        host: host.to_owned(),
        port: section.port,
        bearer: mcp::transport::Bearer::new(token)?,
    }))
}

fn unix_endpoint(section: &TransportSection) -> Result<Option<UnixEndpoint>, SidecarError> {
    let Some(path) = section.socket.as_deref() else {
        return Ok(None);
    };
    absolute_socket(path)?;
    let bearer = match section.auth_token.as_deref() {
        Some(token) => Some(mcp::transport::Bearer::new(token)?),
        None => None,
    };
    Ok(Some(UnixEndpoint {
        path: path.to_path_buf(),
        bearer,
    }))
}

fn absolute_socket(path: &Path) -> Result<(), SidecarError> {
    let traversable = path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir));
    if path.is_absolute() && !traversable {
        Ok(())
    } else {
        Err(SidecarError::Invalid(
            "mcp socket path must be absolute".into(),
        ))
    }
}

async fn serve(
    server: mcp::Server,
    selected: Selected,
    shutdown: CancellationToken,
) -> Result<(), SidecarError> {
    match selected {
        Selected::Stdio => serve_stdio(server, shutdown).await,
        Selected::Network { tcp, unix } => serve_network(server, tcp, unix, shutdown).await,
    }
}

async fn serve_stdio(server: mcp::Server, shutdown: CancellationToken) -> Result<(), SidecarError> {
    tracing::info!("mcp serving stdio");
    let tracker = TaskTracker::new();
    let token = shutdown.child_token();
    let task = tracker.spawn(kernel::tenancy::Scope::carry(async move {
        mcp::transport::serve_stdio(server, token).await
    }));
    let joined = join_task(&shutdown, task).await;
    tracker.close();
    tracker.wait().await;
    joined??;
    Ok(())
}

async fn serve_network(
    server: mcp::Server,
    tcp: Option<TcpEndpoint>,
    unix: Option<UnixEndpoint>,
    shutdown: CancellationToken,
) -> Result<(), SidecarError> {
    let tracker = TaskTracker::new();
    let token = shutdown.clone();
    let driver = tracker.spawn(kernel::tenancy::Scope::carry(async move {
        drive_network(server, tcp, unix, token).await
    }));
    let joined = join_task(&shutdown, driver).await;
    tracker.close();
    tracker.wait().await;
    joined??;
    Ok(())
}

async fn drive_network(
    server: mcp::Server,
    tcp: Option<TcpEndpoint>,
    unix: Option<UnixEndpoint>,
    shutdown: CancellationToken,
) -> Result<(), mcp::Error> {
    let mut tasks = tokio::task::JoinSet::new();
    if let Some(endpoint) = tcp {
        let server = server.clone();
        let token = shutdown.child_token();
        tasks.spawn(kernel::tenancy::Scope::carry(async move {
            listen_tcp(server, endpoint, token).await
        }));
    }
    if let Some(endpoint) = unix {
        let token = shutdown.child_token();
        tasks.spawn(kernel::tenancy::Scope::carry(async move {
            listen_unix(server, endpoint, token).await
        }));
    }
    let mut failure = None;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            joined = tasks.join_next() => {
                match joined {
                    None => break,
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => {
                        failure = Some(error);
                        break;
                    }
                    Some(Err(error)) => {
                        failure = Some(mcp::Error::Task(error));
                        break;
                    }
                }
            }
        }
    }
    shutdown.cancel();
    while let Some(joined) = tasks.join_next().await {
        if failure.is_none() {
            failure = match joined {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) => Some(mcp::Error::Task(error)),
            };
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn listen_tcp(
    server: mcp::Server,
    endpoint: TcpEndpoint,
    shutdown: CancellationToken,
) -> Result<(), mcp::Error> {
    let listener = tokio::net::TcpListener::bind((endpoint.host.as_str(), endpoint.port)).await?;
    let address = listener.local_addr()?;
    tracing::info!("mcp listening on {address}");
    mcp::transport::serve_tcp(listener, server, endpoint.bearer, shutdown).await
}

async fn listen_unix(
    server: mcp::Server,
    endpoint: UnixEndpoint,
    shutdown: CancellationToken,
) -> Result<(), mcp::Error> {
    let bound = mcp::transport::bind_unix(&endpoint.path).await?;
    tracing::info!("mcp listening on {}", endpoint.path.display());
    let transport = mcp::transport::serve_unix(bound, server, endpoint.bearer, shutdown).await;
    let removed = remove_socket(&endpoint.path).await;
    combine(transport, removed)
}

fn combine(
    served: Result<(), mcp::Error>,
    removed: Result<(), mcp::Error>,
) -> Result<(), mcp::Error> {
    match (served, removed) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(served), Err(removed)) => Err(mcp::Error::Invalid(format!("{served}; {removed}"))),
    }
}

async fn remove_socket(path: &Path) -> Result<(), mcp::Error> {
    match tokio::fs::symlink_metadata(path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(mcp::Error::Io(error)),
        Ok(metadata) if metadata.file_type().is_socket() => {
            tokio::fs::remove_file(path).await.map_err(mcp::Error::Io)
        }
        Ok(_) => Err(mcp::Error::SocketNotSocket {
            path: path.to_path_buf(),
        }),
    }
}

async fn join_task<T: Send>(
    shutdown: &CancellationToken,
    task: tokio::task::JoinHandle<T>,
) -> Result<T, tokio::task::JoinError> {
    let _abort = crate::shutdown::AbortOnDrop::arm(&task);
    tokio::pin!(task);
    tokio::select! {
        biased;
        () = shutdown.cancelled() => (&mut task).await,
        result = &mut task => result,
    }
}

pub(crate) fn display_zone(env: &Environment) -> chrono_tz::Tz {
    let configured = env
        .get("TZ")
        .and_then(std::ffi::OsStr::to_str)
        .map(|value| value.trim_start_matches(':').to_owned())
        .filter(|value| !value.is_empty());
    let Some(name) = configured.or_else(zone_from_localtime) else {
        return chrono_tz::Tz::UTC;
    };
    match name.parse::<chrono_tz::Tz>() {
        Ok(zone) => zone,
        Err(error) => {
            tracing::warn!("unknown time zone {name:?} ({error}) — MCP timestamps show UTC");
            chrono_tz::Tz::UTC
        }
    }
}

fn zone_from_localtime() -> Option<String> {
    match std::fs::read_link("/etc/localtime") {
        Ok(target) => {
            let (_, name) = target.to_str()?.split_once("zoneinfo/")?;
            Some(name.to_owned())
        }
        Err(error) => {
            tracing::debug!(%error, "/etc/localtime is not a zone link — MCP timestamps show UTC");
            None
        }
    }
}
