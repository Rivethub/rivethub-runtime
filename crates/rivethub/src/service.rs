use std::io;
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use config::Environment;
use config::Finding;
use config::POSTGRES_URL_VARIABLE;
use config::Profile;
use gateway::Gateway;
use gateway::Settings;
use gateway::server::BindError;
use gateway::server::BindSource;
use gateway::server::ServeError;
use gateway::tls::TlsPaths;
use kernel::memory::MemoryStore;
use kernel::shutdown::Activities;
use memory_postgres::Pooling;
use memory_postgres::PostgresMemory;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::Exit;

const SQLITE_QUERY_EMBED_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("SQLite gateway composition requires a complete SQLite MemoryStore adapter")]
    SqliteUnavailable,
    #[error(transparent)]
    Composition(#[from] crate::composition::ComposeError),
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Node(#[from] crate::NodeError),
    #[error(transparent)]
    Settings(#[from] gateway::SettingsError),
    #[error(
        "no Postgres URL for the memory database — set \"memory.postgres.connection_string\" or \"{POSTGRES_URL_VARIABLE}\""
    )]
    Url,
    #[error(transparent)]
    Memory(#[from] memory_postgres::Error),
    #[error(transparent)]
    Close(#[from] kernel::memory::MemoryStoreError),
    #[error(
        "memory.postgres.embed_model (or RIVETHUB_EMBED_MODEL) is required when an embedding endpoint is set"
    )]
    EmbedModel,
    #[error(
        "memory.sqlite.embed_model (or RIVETHUB_EMBED_MODEL) is required when an embedding endpoint is set"
    )]
    SqliteEmbedModel,
    #[error(transparent)]
    EmbedProvider(#[from] providers::BuildError),
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error(transparent)]
    Serve(#[from] ServeError),
    #[error("the gateway listener failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    State(#[from] config::StateError),
    #[error("the legacy state move task aborted")]
    StateTask(#[source] tokio::task::JoinError),
    #[error(transparent)]
    Voice(#[from] gateway::VoiceConfigError),
}

pub(crate) fn postgres_url(config: &Config, env: &Environment) -> Result<String, StartupError> {
    if config.sqlite_selected() {
        return Err(StartupError::SqliteUnavailable);
    }
    config.memory_postgres_url(env).ok_or(StartupError::Url)
}

pub(crate) async fn postgres(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
    pooling: Pooling,
) -> Result<PostgresMemory, StartupError> {
    let url = postgres_url(config, env)?;
    let embed = query_embed(config, env, shutdown, activities)?;
    let owner = crate::composition::identities::load(config, env)
        .await?
        .primary();
    let memory = PostgresMemory::connect_deferred(&url, pooling)
        .await?
        .with_wiki_directory(config.wiki_directory(env))
        .with_wiki_owner(owner);
    Ok(match embed {
        Some(embed) => memory.with_query_embed(embed),
        None => memory,
    })
}

fn query_embed(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<Option<memory_postgres::QueryEmbed>, StartupError> {
    let Some(endpoint) = config.memory_embed_endpoint(env) else {
        return Ok(None);
    };
    let model = config
        .memory_embed_model(env)
        .ok_or(StartupError::EmbedModel)?;
    let section = config
        .memory
        .as_ref()
        .and_then(|memory| memory.postgres.as_ref());
    let provider = embed_provider(
        config.embed_provider(config::MemoryBackend::Postgres),
        section.and_then(|section| section.embed_api_key.clone()),
        endpoint,
        model,
        shutdown,
        activities,
    )?;
    Ok(Some(memory_postgres::QueryEmbed::new(
        provider,
        section.and_then(|section| section.embed_query_instruction.clone()),
        section.and_then(|section| section.embed_timeout_ms),
        section.and_then(|section| section.hnsw_ef_search),
    )))
}

pub(crate) fn sqlite_vector_model(config: &Config, env: &Environment) -> Option<String> {
    config
        .sqlite_embed_endpoint(env)
        .and_then(|_| config.sqlite_embed_model(env))
}

pub(crate) fn sqlite_query_embed(
    config: &Config,
    env: &Environment,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<Option<memory_sqlite::QueryEmbed>, StartupError> {
    let Some(endpoint) = config.sqlite_embed_endpoint(env) else {
        return Ok(None);
    };
    let model = config
        .sqlite_embed_model(env)
        .ok_or(StartupError::SqliteEmbedModel)?;
    let section = config
        .memory
        .as_ref()
        .and_then(|memory| memory.sqlite.as_ref());
    let api_key = section.and_then(|section| section.embed_api_key.clone());
    let instruction = section
        .and_then(|section| section.embed_query_instruction.clone())
        .unwrap_or_else(|| kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned());
    let provider = embed_provider(
        config.embed_provider(config::MemoryBackend::Sqlite),
        api_key,
        endpoint,
        model,
        shutdown,
        activities,
    )?;
    Ok(Some(memory_sqlite::QueryEmbed::new(
        provider,
        instruction,
        SQLITE_QUERY_EMBED_TIMEOUT,
        shutdown.child_token(),
    )))
}

fn embed_provider(
    provider: Option<&config::provider::OpenaiProvider>,
    api_key: Option<String>,
    endpoint: String,
    model: String,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<Arc<dyn kernel::model::ModelProvider>, StartupError> {
    let auth = provider
        .cloned()
        .unwrap_or_else(|| config::provider::OpenaiProvider {
            api_key,
            ..config::provider::OpenaiProvider::default()
        });
    let provider = providers::OpenAiCompatible::build(
        config::ProviderType::OpenaiCompatible,
        &config::provider::OpenaiProvider {
            base_url: Some(endpoint),
            model: Some(model),
            probe_models: Some(false),
            ..auth
        },
    )?
    .with_shutdown(shutdown.child_token())
    .with_activities(activities.clone());
    Ok(Arc::new(provider))
}

pub async fn connect(
    config: &Config,
    env: &Environment,
) -> Result<Arc<dyn MemoryStore>, StartupError> {
    let shutdown = CancellationToken::new();
    let activities = Activities::new();
    let memory = postgres(config, env, &shutdown, &activities, Pooling::OnDemand).await?;
    Ok(Arc::new(memory))
}

pub async fn compose(
    config: &Config,
    env: &Environment,
    store: Arc<dyn MemoryStore>,
) -> Result<Arc<Gateway>, StartupError> {
    let node = crate::identity::announce_resolved("gateway", config, env)?;
    let settings = Settings::from_config_in(config, env, node.hostname, node.name)?;
    let identity = crate::composition::identities::load(config, env).await?;
    let gateway = Gateway::new(store, settings)
        .with_uploads(uploads(env)?)
        .with_tenancy(identity.tenancy);
    let gateway = with_voice(gateway, env)?;
    Ok(Arc::new(gateway))
}

pub(crate) fn with_voice(
    gateway: Gateway,
    env: &Environment,
) -> Result<Gateway, crate::composition::ComposeError> {
    let voice = env.voice_endpoints();
    Ok(gateway.with_voice(gateway::VoiceSettings::new(
        voice.stt_url,
        voice.tts_url,
        voice.instructions,
    ))?)
}

pub(crate) fn uploads(
    env: &Environment,
) -> Result<gateway::uploads::UploadService, config::ConfigError> {
    let uploads = env.upload_settings();
    for ignored in &uploads.ignored {
        tracing::warn!("ignored upload limit {ignored}");
    }
    Ok(gateway::uploads::UploadService::new(
        gateway::uploads::UploadOptions {
            root: config::Paths::new(env)?.state_dir().join("uploads"),
            max_bytes: uploads.max_bytes,
            ttl: uploads.ttl,
        },
    ))
}

pub async fn listen(
    config: &Config,
    env: &Environment,
) -> Result<(TcpListener, Option<TlsPaths>), StartupError> {
    let node = crate::identity::resolve(config, env)?;
    let section = config.gateway.clone();
    let env = env.clone();
    let shared = config::shared_dir(&env);
    let node_name = node.name;
    let prepared = tokio::task::spawn_blocking(move || {
        let source = BindSource::resolve_section(section.as_ref(), &env, &node_name, &shared);
        let tls = gateway::tls::resolve_tls(&source)?;
        if let Some(paths) = &tls {
            gateway::tls::validate(paths).map_err(ServeError::Tls)?;
        }
        Ok::<_, StartupError>((source, tls))
    })
    .await
    .map_err(StartupError::StateTask)??;
    let listener = gateway::server::bind(&prepared.0).await?;
    Ok((listener, prepared.1))
}

pub async fn serve(
    listener: TcpListener,
    tls: Option<TlsPaths>,
    gateway: Arc<Gateway>,
    shutdown: CancellationToken,
) -> Result<(), StartupError> {
    Ok(gateway::server::serve(listener, gateway, tls, shutdown).await?)
}

pub fn load(env: &Environment) -> Result<Config, StartupError> {
    if let Some(location) = config::resolve_config_path(env)? {
        return Ok(announce(Config::load(env, &location, Profile::Node)?));
    }
    warn_environment(env);
    tracing::warn!("no config file found — running with the built-in defaults");
    Ok(Config::default())
}

pub(crate) fn announce(loaded: config::Loaded) -> Config {
    for finding in &loaded.warnings {
        tracing::warn!("{}", finding_line(finding));
    }
    for notice in &loaded.notices {
        tracing::warn!("{notice}");
    }
    loaded.config
}

fn warn_environment(env: &Environment) {
    for notice in env.legacy_notices() {
        tracing::warn!("{notice}");
    }
    for warning in env.deprecation_warnings() {
        tracing::warn!("{warning}");
    }
}

async fn run(
    env: &Environment,
    shutdown: CancellationToken,
    activities: Activities,
) -> Result<(), StartupError> {
    let _cancel_on_drop = shutdown.clone().drop_guard();
    if let Some(notice) = move_legacy_state(env).await? {
        tracing::warn!("{notice}");
    }
    let owned_env = env.clone();
    let config = tokio::task::spawn_blocking(move || load(&owned_env))
        .await
        .map_err(StartupError::StateTask)??;
    let memory = tokio::select! {
        () = shutdown.cancelled() => return Ok(()),
        memory = postgres(&config, env, &shutdown, &activities, Pooling::Serving) => Arc::new(memory?),
    };
    let scope = crate::composition::identities::load(&config, env)
        .await?
        .owner_scope();
    let reading = Arc::clone(&memory);
    tokio::spawn(scope.run(async move {
        if let Err(error) = reading.stats().await {
            tracing::debug!(%error, "memory stats were not read ahead");
        }
    }));
    let result = run_gateway(&config, env, &memory, &shutdown, &activities).await;
    shutdown.cancel();
    let closed = memory.close().await;
    closed?;
    result?;
    tracing::info!("gateway stopped");
    Ok(())
}

async fn run_gateway(
    config: &Config,
    env: &Environment,
    memory: &Arc<PostgresMemory>,
    shutdown: &CancellationToken,
    activities: &Activities,
) -> Result<(), StartupError> {
    let mut composition =
        crate::composition::Composition::on_postgres(config, env, memory, shutdown, activities)
            .await?;
    let listening = async {
        let (listener, tls) = listen(config, env).await?;
        let addr = listener.local_addr()?;
        tracing::info!(
            "gateway listening on {addr}{}",
            if tls.is_some() { " (tls)" } else { " (plain)" }
        );
        Ok::<_, StartupError>((listener, tls))
    }
    .await;
    let (listener, tls) = match listening {
        Ok(bound) => bound,
        Err(error) => {
            composition.close().await?;
            return Err(error);
        }
    };
    let serving = gateway::server::serve_router_with(
        listener,
        composition.router(tls.is_some()),
        tls,
        shutdown.clone(),
        gateway::server::ServeOptions::default(),
        activities.clone(),
    );
    tokio::pin!(serving);
    let (served, failed) = tokio::select! {
        biased;
        () = shutdown.cancelled() => (None, None),
        result = &mut serving => (Some(result), None),
        result = composition.failed() => (None, Some(result)),
    };
    shutdown.cancel();
    let closed = composition.close().await;
    let served = match served {
        Some(result) => result,
        None => serving.await,
    };
    closed?;
    if let Some(result) = failed {
        result?;
    }
    served?;
    Ok(())
}

pub async fn start(env: &Environment) -> Exit {
    Box::pin(start_with_check(env, false)).await
}

pub async fn start_with_check(env: &Environment, check: bool) -> Exit {
    if check {
        return match check_config(env).await {
            Ok(()) => {
                tracing::info!("gateway configuration valid");
                Exit::Ok
            }
            Err(error) => {
                report(&error);
                error.exit()
            }
        };
    }
    let env = env.clone();
    Box::pin(crate::shutdown::signals_then(
        async move |shutdown, activities| run(&env, shutdown, activities).await,
        |result| match result {
            Ok(()) => Exit::Ok,
            Err(error) => {
                report(&error);
                error.exit()
            }
        },
    ))
    .await
}

async fn check_config(env: &Environment) -> Result<(), StartupError> {
    let loading_env = env.clone();
    let config = tokio::task::spawn_blocking(move || {
        let config = load(&loading_env)?;
        let node = crate::identity::resolve(&config, &loading_env)?;
        let source = BindSource::resolve(
            &config,
            &loading_env,
            &node.name,
            &config::shared_dir(&loading_env),
        );
        if let Some(tls) = gateway::tls::resolve_tls(&source)? {
            gateway::tls::validate(&tls).map_err(ServeError::Tls)?;
        }
        Ok::<_, StartupError>(config)
    })
    .await
    .map_err(StartupError::StateTask)??;
    crate::composition::Composition::validate_users(&config, env).await?;
    crate::identity::announce_resolved("gateway", &config, env)?;
    Ok(())
}

async fn move_legacy_state(env: &Environment) -> Result<Option<String>, StartupError> {
    let env = env.clone();
    let moved = tokio::task::spawn_blocking(move || config::adopt_legacy_state(&env))
        .await
        .map_err(StartupError::StateTask)?;
    Ok(moved?)
}

pub(crate) fn report(error: &StartupError) {
    if let StartupError::Config(config::ConfigError::Validation { result }) = error {
        for finding in &result.errors {
            tracing::error!("{}", finding_line(finding));
        }
        return;
    }
    tracing::error!("{error}");
}

fn finding_line(finding: &Finding) -> String {
    if finding.path.is_empty() {
        finding.message.clone()
    } else {
        format!("[{}] {}", finding.path, finding.message)
    }
}

#[cfg(test)]
mod embed_tests {
    use super::*;

    fn searched(
        config: &Config,
        env: &Environment,
    ) -> Result<Option<memory_postgres::QueryEmbed>, StartupError> {
        query_embed(config, env, &CancellationToken::new(), &Activities::new())
    }

    fn config(memory: &str) -> Result<Config, Box<dyn std::error::Error>> {
        let text = format!(
            "runtime:\n  workspace: /tmp\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:9/v1\n    model: m\nmemory:\n  backend: postgres\n  postgres:\n    connection_string: postgres://u@127.0.0.1:9/db\n{memory}"
        );
        Ok(Config::from_str(&Environment::default(), &text)?.config)
    }

    #[test]
    fn without_an_endpoint_search_stays_keyword_only() -> Result<(), Box<dyn std::error::Error>> {
        assert!(searched(&config("")?, &Environment::default())?.is_none());
        Ok(())
    }

    #[test]
    fn an_endpoint_without_a_model_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let refused = searched(
            &config("    embed_endpoint: http://127.0.0.1:9\n")?,
            &Environment::default(),
        );
        assert!(matches!(refused, Err(StartupError::EmbedModel)));
        Ok(())
    }

    #[test]
    fn an_endpoint_and_model_give_search_a_query_embedder() -> Result<(), Box<dyn std::error::Error>>
    {
        let embed = searched(
            &config(
                "    embed_endpoint: http://127.0.0.1:9\n    embed_model: text-embedding-3-small\n",
            )?,
            &Environment::default(),
        )?;
        assert!(embed.is_some());
        Ok(())
    }

    #[test]
    fn the_environment_supplies_the_model_like_the_den() -> Result<(), Box<dyn std::error::Error>> {
        let env = Environment::from_vars([(
            "RIVETOS_EMBED_MODEL".to_owned(),
            "qwen3-embedding-0.6b".to_owned(),
        )]);
        let embed = searched(&config("    embed_endpoint: http://127.0.0.1:9\n")?, &env)?;
        assert!(embed.is_some());
        let env = Environment::from_vars([
            (
                "RIVETHUB_EMBED_URL".to_owned(),
                "http://127.0.0.1:9".to_owned(),
            ),
            (
                "RIVETHUB_EMBED_MODEL".to_owned(),
                "qwen3-embedding-0.6b".to_owned(),
            ),
        ]);
        assert!(searched(&config("")?, &env)?.is_some());
        Ok(())
    }
}

#[cfg(test)]
mod sqlite_embed_tests {
    use super::*;

    fn sqlite_config(memory: &str) -> Result<Config, Box<dyn std::error::Error>> {
        let text = format!(
            "runtime:\n  workspace: /tmp\n  default_agent: main\nagents:\n  main:\n    provider: p\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:9/v1\n    model: m\nmemory:\n  backend: sqlite\n  sqlite:\n    path: /tmp/memory.sqlite\n{memory}"
        );
        Ok(Config::from_str(&Environment::default(), &text)?.config)
    }

    fn searched(
        memory: &str,
        env: &Environment,
    ) -> Result<Option<memory_sqlite::QueryEmbed>, Box<dyn std::error::Error>> {
        Ok(sqlite_query_embed(
            &sqlite_config(memory)?,
            env,
            &CancellationToken::new(),
            &Activities::new(),
        )?)
    }

    #[test]
    fn without_an_endpoint_sqlite_search_stays_keyword_only()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(searched("", &Environment::default())?.is_none());
        Ok(())
    }

    #[test]
    fn a_sqlite_endpoint_without_a_model_is_refused() {
        let refused = searched(
            "    embed_endpoint: http://127.0.0.1:9\n",
            &Environment::default(),
        );
        assert!(
            refused
                .err()
                .and_then(|error| error.downcast::<StartupError>().ok())
                .is_some_and(|error| matches!(*error, StartupError::SqliteEmbedModel))
        );
    }

    #[test]
    fn the_sqlite_section_or_the_environment_arms_the_query_embedder()
    -> Result<(), Box<dyn std::error::Error>> {
        let configured = searched(
            "    embed_endpoint: http://127.0.0.1:9\n    embed_model: qwen3-embedding-0.6b\n",
            &Environment::default(),
        )?;
        assert!(configured.is_some());
        let env = Environment::from_vars([
            (
                "RIVETHUB_EMBED_URL".to_owned(),
                "http://127.0.0.1:9".to_owned(),
            ),
            (
                "RIVETHUB_EMBED_MODEL".to_owned(),
                "qwen3-embedding-0.6b".to_owned(),
            ),
        ]);
        assert!(searched("", &env)?.is_some());
        Ok(())
    }

    #[test]
    fn the_vector_model_is_the_configured_sqlite_embedder() -> Result<(), Box<dyn std::error::Error>>
    {
        let env = Environment::default();
        assert_eq!(sqlite_vector_model(&sqlite_config("")?, &env), None);
        let model_only = sqlite_config("    embed_model: embed-a\n")?;
        assert_eq!(sqlite_vector_model(&model_only, &env), None);
        let armed =
            sqlite_config("    embed_endpoint: http://127.0.0.1:9\n    embed_model: embed-a\n")?;
        assert_eq!(
            sqlite_vector_model(&armed, &env).as_deref(),
            Some("embed-a")
        );
        Ok(())
    }
}
