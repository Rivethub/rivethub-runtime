use serde::Deserialize;
use thiserror::Error;

use crate::Environment;
use crate::enums::{Effort, MemoryBackend};
use crate::model::Config;
use crate::provider::ProviderBody;

pub const NO_MODEL_NOTE: &str = "No model is configured. This node captures, stores and serves recall. Summaries, titles, embeddings, wiki pages and entities stay off until a model is set.";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct EmbedderChoice {
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MemoryModels {
    pub default: ModelChoice,
    #[serde(default)]
    pub summarizer: Option<ModelChoice>,
    #[serde(default)]
    pub categorizer: Option<ModelChoice>,
    #[serde(default)]
    pub wiki_writer: Option<ModelChoice>,
    #[serde(default)]
    pub embedder: Option<EmbedderChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inherited<T> {
    pub choice: T,
    pub inherits_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModels {
    pub derived: bool,
    pub default: ModelChoice,
    pub summarizer: Inherited<ModelChoice>,
    pub categorizer: Inherited<ModelChoice>,
    pub wiki_writer: Inherited<ModelChoice>,
    pub embedder: Inherited<Option<EmbedderChoice>>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelsError {
    #[error("runtime is required for providers")]
    Runtime,
    #[error("default_agent is required")]
    DefaultAgent,
    #[error("agents are required for providers")]
    Agents,
    #[error("default agent {0} is missing")]
    MissingAgent(String),
    #[error("the default agent requires a provider")]
    Provider,
}

impl ResolvedModels {
    fn explicit(models: MemoryModels, derived: Option<EmbedderChoice>) -> Self {
        let inherit = |choice: Option<ModelChoice>| match choice {
            Some(choice) => Inherited {
                choice,
                inherits_default: false,
            },
            None => Inherited {
                choice: models.default.clone(),
                inherits_default: true,
            },
        };
        Self {
            derived: false,
            summarizer: inherit(models.summarizer.clone()),
            categorizer: inherit(models.categorizer.clone()),
            wiki_writer: inherit(models.wiki_writer.clone()),
            embedder: match models.embedder {
                Some(choice) => Inherited {
                    choice: Some(choice),
                    inherits_default: false,
                },
                None => Inherited {
                    choice: derived,
                    inherits_default: true,
                },
            },
            default: models.default,
        }
    }

    fn inheriting(default: ModelChoice, embedder: Option<EmbedderChoice>) -> Self {
        let inherited = || Inherited {
            choice: default.clone(),
            inherits_default: true,
        };
        Self {
            derived: true,
            summarizer: inherited(),
            categorizer: inherited(),
            wiki_writer: inherited(),
            embedder: Inherited {
                choice: embedder,
                inherits_default: true,
            },
            default,
        }
    }
}

impl Config {
    pub fn memory_models(&self, env: &Environment) -> Result<ResolvedModels, ModelsError> {
        self.memory_models_from(env.embed_url(), env.embed_model())
    }

    pub fn memory_models_from(
        &self,
        env_endpoint: Option<String>,
        env_model: Option<String>,
    ) -> Result<ResolvedModels, ModelsError> {
        let embedder = self.derived_embedder(env_endpoint, env_model);
        match self
            .memory
            .as_ref()
            .and_then(|memory| memory.models.clone())
        {
            Some(models) => Ok(ResolvedModels::explicit(models, embedder)),
            None => Ok(ResolvedModels::inheriting(
                self.default_agent_choice()?,
                embedder,
            )),
        }
    }

    #[must_use]
    pub fn memory_models_available(&self) -> bool {
        self.memory
            .as_ref()
            .is_some_and(|memory| memory.models.is_some())
            || self.default_agent_choice().is_ok()
    }

    pub fn default_agent_choice(&self) -> Result<ModelChoice, ModelsError> {
        let runtime = self.runtime.as_ref().ok_or(ModelsError::Runtime)?;
        let name = runtime
            .default_agent
            .as_ref()
            .ok_or(ModelsError::DefaultAgent)?;
        let agents = self.agents.as_ref().ok_or(ModelsError::Agents)?;
        let agent = agents
            .get(name)
            .ok_or_else(|| ModelsError::MissingAgent(name.clone()))?;
        Ok(ModelChoice {
            provider: agent.provider.clone().ok_or(ModelsError::Provider)?,
            model: agent.model.clone(),
            effort: None,
        })
    }

    #[must_use]
    pub fn model_named(&self, choice: &ModelChoice) -> Option<String> {
        choice
            .model
            .clone()
            .or_else(|| self.provider_model(&choice.provider))
    }

    #[must_use]
    pub fn embedder_model_named(&self, choice: &EmbedderChoice) -> Option<String> {
        choice.model.clone().or_else(|| {
            choice
                .provider
                .as_deref()
                .and_then(|id| self.provider_model(id))
        })
    }

    fn provider_model(&self, id: &str) -> Option<String> {
        match &self.providers.as_ref()?.get(id)?.body {
            ProviderBody::Openai(body) => body.model.clone(),
            ProviderBody::Cli(body) => body.model.clone(),
            ProviderBody::Unknown(_) | ProviderBody::Removed(_) => None,
        }
    }

    #[must_use]
    pub fn derived_embedder(
        &self,
        env_endpoint: Option<String>,
        env_model: Option<String>,
    ) -> Option<EmbedderChoice> {
        let backend = if self.sqlite_selected() {
            MemoryBackend::Sqlite
        } else {
            MemoryBackend::Postgres
        };
        let memory = self.memory.as_ref()?;
        let (endpoint, model, provider) = match backend {
            MemoryBackend::Sqlite => {
                let section = memory.sqlite.as_ref();
                (
                    section.and_then(|section| section.embed_endpoint.as_deref()),
                    section.and_then(|section| section.embed_model.as_deref()),
                    section.and_then(|section| section.embed_provider.as_deref()),
                )
            }
            MemoryBackend::Postgres => {
                let section = memory.postgres.as_ref();
                (
                    section.and_then(|section| section.embed_endpoint.as_deref()),
                    section.and_then(|section| section.embed_model.as_deref()),
                    section.and_then(|section| section.embed_provider.as_deref()),
                )
            }
        };
        let text = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let provider = text(provider).filter(|_| self.embed_provider_url(backend).is_some());
        let endpoint = text(endpoint).or(env_endpoint);
        (provider.is_some() || endpoint.is_some()).then(|| EmbedderChoice {
            provider,
            endpoint,
            model: text(model).or(env_model),
        })
    }
}
