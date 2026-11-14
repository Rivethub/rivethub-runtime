use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use config::provider::{ProviderBody, ProviderSlot};
use config::{Config, EmbedderChoice, Environment, ModelChoice, ModelsError, ResolvedModels};
use harness_acp::{HeadlessLaunch, Retry, Supervisor};
use kernel::harness::HarnessAdapter;
use kernel::model::{ModelProvider, ProviderHealth};
use kernel::shutdown::Activities;
use thiserror::Error;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("provider {0} is missing")]
    Missing(String),
    #[error("provider {0} has no supported type")]
    Type(String),
    #[error("provider {0} requires a model on the provider or agent")]
    Model(String),
    #[error("headless provider startup failed: {0}")]
    Startup(#[from] tokio::task::JoinError),
    #[error("provider {0} has no composed harness adapter")]
    Harness(String),
    #[error(transparent)]
    Provider(#[from] providers::BuildError),
    #[error(transparent)]
    Cli(#[from] providers::cli::CliError),
    #[error(transparent)]
    Paths(#[from] config::ConfigError),
}

#[expect(
    missing_debug_implementations,
    reason = "kernel harness ports do not implement Debug"
)]
#[derive(Default)]
pub struct ModelFactory {
    harnesses: BTreeMap<String, Arc<dyn HarnessAdapter>>,
    health: BTreeMap<String, ProviderHealth>,
    tasks: TaskTracker,
    cancel: CancellationToken,
    embed_endpoint: Option<String>,
    embed_model: Option<String>,
    activities: Activities,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelTasks {
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl ModelTasks {
    pub(crate) async fn close(&self) {
        self.cancel.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

#[expect(
    missing_debug_implementations,
    reason = "kernel model ports do not implement Debug"
)]
pub struct ModelBinding {
    pub provider: Arc<dyn ModelProvider>,
    pub model: String,
    pub embeddings: bool,
}

struct EmbedExtras {
    api_key: Option<String>,
    expected_dims: Option<u32>,
    timeout_ms: Option<u64>,
    query_instruction: Option<String>,
}

impl EmbedExtras {
    fn of(config: &Config) -> Self {
        let memory = config.memory.as_ref();
        if config.sqlite_selected() {
            let section = memory.and_then(|memory| memory.sqlite.as_ref());
            Self {
                api_key: section.and_then(|section| section.embed_api_key.clone()),
                expected_dims: None,
                timeout_ms: None,
                query_instruction: None,
            }
        } else {
            let section = memory.and_then(|memory| memory.postgres.as_ref());
            Self {
                api_key: section.and_then(|section| section.embed_api_key.clone()),
                expected_dims: section.and_then(|section| section.embed_expected_dims),
                timeout_ms: section.and_then(|section| section.embed_timeout_ms),
                query_instruction: section
                    .and_then(|section| section.embed_query_instruction.clone()),
            }
        }
    }
}

impl ModelFactory {
    fn with_embedding(env: &Environment) -> Self {
        Self {
            embed_endpoint: env.embed_url(),
            embed_model: env.embed_model(),
            ..Self::default()
        }
    }

    fn embedder(&self, config: &Config) -> Result<Option<providers::embed::Embedder>, ModelError> {
        self.derived_embedder(config)
            .map(|choice| Self::embedder_for(config, &choice).map(|(embedder, _)| embedder))
            .transpose()
    }

    fn derived_embedder(&self, config: &Config) -> Option<EmbedderChoice> {
        config.derived_embedder(self.embed_endpoint.clone(), self.embed_model.clone())
    }

    pub fn resolve_models(&self, config: &Config) -> Result<ResolvedModels, ModelsError> {
        config.memory_models_from(self.embed_endpoint.clone(), self.embed_model.clone())
    }

    fn embedder_for(
        config: &Config,
        choice: &EmbedderChoice,
    ) -> Result<(providers::embed::Embedder, String), ModelError> {
        let provider = match &choice.provider {
            Some(id) => match &slot(config, id)?.body {
                ProviderBody::Openai(provider) => Some(provider.as_ref()),
                ProviderBody::Cli(_) | ProviderBody::Unknown(_) | ProviderBody::Removed(_) => {
                    return Err(ModelError::Type(id.clone()));
                }
            },
            None => None,
        };
        let text = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let endpoint = text(provider.and_then(|provider| provider.base_url.as_deref()))
            .or_else(|| text(choice.endpoint.as_deref()))
            .ok_or(providers::BuildError::MissingEmbedEndpoint)?;
        let model = text(choice.model.as_deref())
            .or_else(|| text(provider.and_then(|provider| provider.model.as_deref())))
            .ok_or(providers::BuildError::MissingEmbedModel)?;
        let extras = EmbedExtras::of(config);
        let embedder = providers::embed::Embedder::build(providers::embed::EmbedConfig {
            endpoint,
            model: model.clone(),
            api_key: extras.api_key.filter(|_| provider.is_none()),
            api_prefix: provider.and_then(|provider| provider.api_prefix.clone()),
            expected_dims: extras.expected_dims,
            timeout_ms: extras.timeout_ms,
            query_instruction: extras.query_instruction,
        })?;
        let embedder = match provider {
            Some(provider) => embedder.with_provider_auth(provider)?,
            None => embedder,
        };
        Ok((embedder, model))
    }

    pub fn embedding_provider(
        config: &Config,
        choice: &EmbedderChoice,
    ) -> Result<Arc<dyn ModelProvider>, ModelError> {
        let (embedder, model) = Self::embedder_for(config, choice)?;
        Ok(Arc::new(providers::EmbedProvider::new(embedder, model)))
    }

    pub(crate) fn tasks(&self) -> ModelTasks {
        ModelTasks {
            tasks: self.tasks.clone(),
            cancel: self.cancel.clone(),
        }
    }

    #[must_use]
    pub fn health(&self) -> Vec<ProviderHealth> {
        self.health.values().cloned().collect()
    }

    #[must_use]
    pub fn without_harnesses(env: &Environment) -> Self {
        Self::with_embedding(env)
    }

    pub async fn for_providers(
        config: &Config,
        env: &Environment,
        ids: BTreeSet<&str>,
        shutdown: &CancellationToken,
    ) -> Result<Self, ModelError> {
        Self::with_embedding(env)
            .with_providers(config, env, ids, shutdown)
            .await
    }

    #[must_use]
    pub fn with_harness(mut self, id: String, harness: Arc<dyn HarnessAdapter>) -> Self {
        self.harnesses.insert(id, harness);
        self
    }

    #[must_use]
    pub fn with_activities(mut self, activities: Activities) -> Self {
        self.activities = activities;
        self
    }

    pub async fn with_providers(
        mut self,
        config: &Config,
        env: &Environment,
        ids: BTreeSet<&str>,
        shutdown: &CancellationToken,
    ) -> Result<Self, ModelError> {
        self.cancel = shutdown.child_token();
        let supervisor = Supervisor {
            shutdown: self.cancel.clone(),
            tasks: self.tasks.clone(),
            retry: Retry::STANDARD,
        };
        let search: Vec<_> = match env.get("PATH") {
            Some(paths) => std::env::split_paths(paths).collect(),
            None => Vec::new(),
        };
        let mut starting = JoinSet::new();
        for id in ids {
            if self.harnesses.contains_key(id) {
                continue;
            }
            let slot = slot(config, id)?;
            if let ProviderBody::Cli(body) = &slot.body {
                let kind = slot.kind.ok_or_else(|| ModelError::Type(id.into()))?;
                let harness = providers::cli::harness_id(kind)?;
                let launch =
                    HeadlessLaunch::prepare(config, harness, body, search.clone(), id.into())
                        .await
                        .with_discovery_dir(config::Paths::new(env)?.discovery_dir());
                starting.spawn(launch.start(supervisor.clone()));
            }
        }
        while let Some(started) = starting.join_next().await {
            let harness = started?;
            let health = harness.health();
            self.harnesses.insert(health.id.clone(), Arc::new(harness));
            self.health.insert(health.id.clone(), health);
        }
        Ok(self)
    }

    pub fn build(
        &self,
        config: &Config,
        id: &str,
        model: Option<&str>,
        shutdown: &CancellationToken,
    ) -> Result<ModelBinding, ModelError> {
        self.build_with(config, id, model, None, shutdown)
    }

    pub fn build_choice(
        &self,
        config: &Config,
        choice: &ModelChoice,
        shutdown: &CancellationToken,
    ) -> Result<ModelBinding, ModelError> {
        self.build_with(
            config,
            &choice.provider,
            choice.model.as_deref(),
            choice.effort,
            shutdown,
        )
    }

    fn build_with(
        &self,
        config: &Config,
        id: &str,
        model: Option<&str>,
        effort: Option<config::Effort>,
        shutdown: &CancellationToken,
    ) -> Result<ModelBinding, ModelError> {
        let slot = slot(config, id)?;
        let kind = slot.kind.ok_or_else(|| ModelError::Type(id.into()))?;
        match &slot.body {
            ProviderBody::Openai(body) => {
                let mut provider = providers::OpenAiCompatible::build(kind, body)?
                    .with_shutdown(shutdown.child_token())
                    .with_activities(self.activities.clone());
                if let Some(embedder) = self.embedder(config)? {
                    provider = provider.with_embedder(embedder);
                }
                let model = match model {
                    Some(model) => model.to_owned(),
                    None => provider.model().to_owned(),
                };
                Ok(ModelBinding {
                    provider: Arc::new(provider),
                    model,
                    embeddings: true,
                })
            }
            ProviderBody::Cli(body) => {
                let model = model
                    .or(body.model.as_deref())
                    .filter(|model| !model.is_empty())
                    .ok_or_else(|| ModelError::Model(id.into()))?
                    .to_owned();
                let harness = self
                    .harnesses
                    .get(id)
                    .ok_or_else(|| ModelError::Harness(id.into()))?;
                let mut options = body.as_ref().clone();
                options.reasoning_effort = effort.or(options.reasoning_effort);
                options.cwd = options.cwd.or_else(|| {
                    config
                        .runtime
                        .as_ref()
                        .and_then(|runtime| runtime.workspace.clone())
                });
                let embedder = self.embedder(config)?;
                let embeddings = embedder.is_some();
                let provider = providers::cli::CliBacked::build(
                    kind,
                    options,
                    harness.clone(),
                    embedder,
                    shutdown,
                )?
                .with_task_tracker(self.tasks.clone());
                Ok(ModelBinding {
                    provider: Arc::new(provider),
                    model,
                    embeddings,
                })
            }
            ProviderBody::Unknown(_) | ProviderBody::Removed(_) => Err(ModelError::Type(id.into())),
        }
    }
}

fn slot<'a>(config: &'a Config, id: &str) -> Result<&'a ProviderSlot, ModelError> {
    config
        .providers
        .as_ref()
        .and_then(|slots| slots.get(id))
        .ok_or_else(|| ModelError::Missing(id.into()))
}
