use std::path::PathBuf;

use serde::Deserialize;

use crate::enums::{Effort, MemoryBackend};
use crate::model::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum ProviderType {
    OpenaiCompatible,
    Vllm,
    ClaudeCli,
    CodexCli,
    GrokCli,
    HermesCli,
    KimiCode,
    OpencodeCli,
}

impl ProviderType {
    pub fn is_openai_family(self) -> bool {
        matches!(self, Self::OpenaiCompatible | Self::Vllm)
    }

    pub fn is_cli_family(self) -> bool {
        matches!(
            self,
            Self::ClaudeCli
                | Self::CodexCli
                | Self::GrokCli
                | Self::HermesCli
                | Self::KimiCode
                | Self::OpencodeCli
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString, strum::EnumIter)]
#[strum(serialize_all = "kebab-case")]
pub enum RemovedProviderType {
    Ollama,
    LlamaServer,
    OpenaiCompat,
    Cli,
    PiCli,
    QwenCode,
    Anthropic,
    Google,
    Gemini,
    Xai,
}

impl RemovedProviderType {
    pub fn message(self) -> &'static str {
        match self {
            Self::Ollama => {
                "reach Ollama through a harness, or use type: openai-compatible with base_url http://<host>:11434/v1"
            }
            Self::LlamaServer => "use type: openai-compatible with base_url <server>/v1",
            Self::OpenaiCompat => "spelled openai-compatible",
            Self::Cli | Self::PiCli | Self::QwenCode => {
                "no CLI provider of that type; use a harness"
            }
            Self::Anthropic => "reach anthropic models through the claude-code harness instead",
            Self::Google => "reach google models through the gemini harness instead",
            Self::Gemini => "reach gemini models through the gemini harness instead",
            Self::Xai => "reach xai models through the grok-build harness instead",
        }
    }
}

#[derive(Debug)]
pub struct ProviderSlot {
    pub kind: Option<ProviderType>,
    pub body: ProviderBody,
}

#[derive(Debug)]
pub enum ProviderBody {
    Openai(Box<OpenaiProvider>),
    Cli(Box<CliProvider>),
    Unknown(UnknownProvider),
    Removed(RemovedProviderType),
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct OpenaiProvider {
    pub model: Option<String>,
    pub name: Option<String>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    #[serde(rename = "type")]
    pub declared: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,

    pub api_key_file: Option<PathBuf>,

    pub reasoning: Option<bool>,
    pub token_command: Option<Vec<String>>,
    pub token_ttl_ms: Option<u64>,
    pub token_command_timeout_ms: Option<u64>,
    pub api_prefix: Option<String>,
    pub max_tokens: Option<u32>,
    pub probe_models: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct CliProvider {
    pub model: Option<String>,
    pub name: Option<String>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    #[serde(rename = "type")]
    pub declared: Option<String>,
    pub binary: Option<String>,
    pub tools: Option<String>,
    pub effort: Option<Effort>,
    pub permission_mode: Option<String>,
    pub exclude_dynamic_sections: Option<bool>,
    pub append_system_prompt: Option<bool>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: Option<u64>,
    pub discovery_timeout_ms: Option<u64>,
    pub permission_prompts: Option<String>,
    pub permission_timeout_ms: Option<u64>,
    pub allowed_api_key_sources: Option<Vec<String>>,
    pub home: Option<PathBuf>,
    pub session: Option<String>,
    pub profile: Option<String>,
    pub sandbox: Option<String>,
    pub approve_for_me: Option<bool>,
    pub skip_git_repo_check: Option<bool>,
    pub reasoning_effort: Option<Effort>,
    pub max_turns: Option<u32>,
    pub no_plan: Option<bool>,
    pub system_prompt: Option<String>,
    pub allow: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct UnknownProvider {
    #[serde(rename = "type")]
    pub declared: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub api_key: Option<String>,
}

impl Config {
    #[must_use]
    pub fn embed_provider(&self, backend: MemoryBackend) -> Option<&OpenaiProvider> {
        let memory = self.memory.as_ref()?;
        let id = match backend {
            MemoryBackend::Postgres => memory.postgres.as_ref()?.embed_provider.as_deref(),
            MemoryBackend::Sqlite => memory.sqlite.as_ref()?.embed_provider.as_deref(),
        }?;
        match &self.providers.as_ref()?.get(id)?.body {
            ProviderBody::Openai(provider) => Some(provider),
            ProviderBody::Cli(_) | ProviderBody::Unknown(_) | ProviderBody::Removed(_) => None,
        }
    }

    #[must_use]
    pub fn embed_provider_url(&self, backend: MemoryBackend) -> Option<String> {
        self.embed_provider(backend)
            .and_then(|provider| provider.base_url.as_deref())
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::OpenaiProvider;

    #[test]
    fn openrouter_fields_deserialize() {
        let text = "\
base_url: https://openrouter.ai/api/v1
model: qwen/qwen3.8-flash
api_key: ${OPENROUTER_API_KEY}
api_key_file: /run/secrets/openrouter_api_key
reasoning: false
";
        let provider: OpenaiProvider = serde_yaml_ng::from_str(text).expect("provider");
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(provider.model.as_deref(), Some("qwen/qwen3.8-flash"));
        assert_eq!(provider.api_key.as_deref(), Some("${OPENROUTER_API_KEY}"));
        assert_eq!(
            provider.api_key_file,
            Some(PathBuf::from("/run/secrets/openrouter_api_key"))
        );
        assert_eq!(provider.reasoning, Some(false));
    }
}
