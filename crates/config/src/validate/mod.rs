mod checks;

use checks::{
    hardcoded_key_warning, looks_like_api_key, non_empty_entries, positive, required_path,
    required_string,
};

mod agents;
mod channels;
mod deployment;
mod entities;
mod gateway;
mod hubs;
mod memory;
mod providers;
pub mod retired;
mod runtime;
mod tasks;

use strum::IntoEnumIterator as _;

use crate::model::{AgentEntry, Config};
use crate::report::{Finding, ValidationResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredPath(Vec<Segment>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Key(String),
    Index(usize),
}

impl IgnoredPath {
    pub(super) fn collect(prefix: &str, path: &serde_ignored::Path<'_>) -> Self {
        let mut collected = Vec::new();
        collect_segments(&mut collected, path);
        collected.reverse();
        let mut segments: Vec<Segment> = prefix
            .split('.')
            .filter(|part| !part.is_empty())
            .map(|part| Segment::Key(part.to_owned()))
            .collect();
        segments.extend(collected);
        Self(segments)
    }

    fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for segment in &self.0 {
            let part = match segment {
                Segment::Key(key) => key.clone(),
                Segment::Index(index) => index.to_string(),
            };
            if part.bytes().all(|byte| byte.is_ascii_digit())
                && let Some(last) = parts.last_mut()
            {
                last.push('[');
                last.push_str(&part);
                last.push(']');
            } else {
                parts.push(part);
            }
        }
        parts.join(".")
    }
}

impl std::fmt::Display for IgnoredPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

fn collect_segments(segments: &mut Vec<Segment>, path: &serde_ignored::Path<'_>) {
    match path {
        serde_ignored::Path::Root => {}
        serde_ignored::Path::Seq { parent, index } => {
            segments.push(Segment::Index(*index));
            collect_segments(segments, parent);
        }
        serde_ignored::Path::Map { parent, key } => {
            let key = normalized_key(key);
            if !key.is_empty() {
                segments.push(Segment::Key(key));
            }
            collect_segments(segments, parent);
        }
        serde_ignored::Path::Some { parent }
        | serde_ignored::Path::NewtypeStruct { parent }
        | serde_ignored::Path::NewtypeVariant { parent } => collect_segments(segments, parent),
    }
}

fn normalized_key(key: &str) -> String {
    match key.rsplit_once('?') {
        Some((name, index))
            if !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            format!("{name}[{index}]")
        }
        _ => key.replace('?', ""),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
enum RemovedKey {
    #[strum(
        serialize = "Provider fallback was removed in the AI SDK migration. Remove \"runtime.fallbacks\" from your config."
    )]
    RuntimeFallbacks,
    #[strum(
        serialize = "The coding-pipeline plugin was removed. Remove \"runtime.coding_pipeline\"; use delegate_task (and the upcoming task engine) instead."
    )]
    RuntimeCodingPipeline,
    #[strum(
        serialize = "Per-agent fallback chains were removed in the AI SDK migration. Remove \"fallbacks\" from this agent."
    )]
    AgentFallbacks,
    #[strum(
        serialize = "The background review loop was removed — the summarizer is the single consolidation path. Remove \"review_endpoint\"; for delegation-event persistence set \"delegation_tracking: true\"."
    )]
    PostgresReviewEndpoint,
    #[strum(
        serialize = "The background review loop was removed. Remove \"review_model\" from memory.postgres."
    )]
    PostgresReviewModel,
    #[strum(
        serialize = "The background review loop was removed. Remove \"review_api_key\" from memory.postgres."
    )]
    PostgresReviewApiKey,
}

impl RemovedKey {
    fn from_path(segments: &[Segment]) -> Option<Self> {
        let [Segment::Key(head), rest @ ..] = segments else {
            return None;
        };
        let head: &str = head.as_str();
        match (head, rest) {
            ("runtime", [Segment::Key(key)]) if key == "fallbacks" => Some(Self::RuntimeFallbacks),
            ("runtime", [Segment::Key(key)]) if key == "coding_pipeline" => {
                Some(Self::RuntimeCodingPipeline)
            }
            ("memory", [Segment::Key(parent), Segment::Key(key)]) if parent == "postgres" => {
                let key: &str = key.as_str();
                match key {
                    "review_endpoint" => Some(Self::PostgresReviewEndpoint),
                    "review_model" => Some(Self::PostgresReviewModel),
                    "review_api_key" => Some(Self::PostgresReviewApiKey),
                    _ => None,
                }
            }
            ("agents", [Segment::Key(_), Segment::Key(key)]) if key == "fallbacks" => {
                Some(Self::AgentFallbacks)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumIter)]
#[strum(serialize_all = "snake_case")]
enum RemovedProviderKey {
    Temperature,
    TopP,
    TopK,
    MinP,
    PresencePenalty,
    FrequencyPenalty,
    Seed,
    Stop,
    RepetitionPenalty,
    MinTokens,
    DefaultToolChoice,
    MmProcessorKwargs,
    ChatTemplateKwargs,
    ExtraBody,
    Models,
    ModelsTtlMs,
    ModelsUrl,
    VerifyModelOnInit,
    NumCtx,
    KeepAlive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumIter)]
enum RemovedEmbedKey {
    #[strum(serialize = "embed_token_command")]
    TokenCommand,
    #[strum(serialize = "embed_token_ttl_ms")]
    TokenTtlMs,
    #[strum(serialize = "embed_token_command_timeout_ms")]
    TokenCommandTimeoutMs,
    #[strum(serialize = "embed_wire_shape")]
    WireShape,
}

fn removed_key_warning(path: String, key: &impl std::fmt::Display) -> Finding {
    Finding::warning(
        path,
        format!("{key} is set on the model server, not in RivetHub"),
    )
}

fn removed_key_finding(
    config: &Config,
    path: &IgnoredPath,
    segments: &[Segment],
) -> Option<Finding> {
    match segments {
        [Segment::Key(head), Segment::Key(id), Segment::Key(key)] if head == "providers" => {
            let key = RemovedProviderKey::iter().find(|candidate| candidate.to_string() == *key)?;
            let slot = config.providers.as_ref()?.get(id)?;
            slot.kind?;
            Some(removed_key_warning(path.to_string(), &key))
        }
        [Segment::Key(head), Segment::Key(section), Segment::Key(key)]
            if head == "memory" && (section == "postgres" || section == "sqlite") =>
        {
            Some(removed_key_warning(
                path.to_string(),
                &RemovedEmbedKey::iter().find(|candidate| candidate.to_string() == *key)?,
            ))
        }
        [
            Segment::Key(head),
            Segment::Key(grand),
            Segment::Key(parent),
            Segment::Key(key),
        ] if head == "memory"
            && grand == "postgres"
            && parent == "embedded"
            && key == "auto_migrate" =>
        {
            Some(Finding::warning(
                path.to_string(),
                "auto_migrate is gone; run rivethub-migrate",
            ))
        }
        [
            Segment::Key(head),
            Segment::Key(grand),
            Segment::Key(parent),
            Segment::Key(key),
        ] if head == "memory" && grand == "postgres" && parent == "embedded" => {
            Some(removed_key_warning(
                path.to_string(),
                &RemovedEmbedKey::iter().find(|candidate| candidate.to_string() == *key)?,
            ))
        }
        [Segment::Key(head), Segment::Key(key), ..] if head == "tasks" && key == "pricing" => {
            Some(Finding::warning(
                path.to_string(),
                "\"tasks.pricing\" is no longer used: maxUsd budgets were removed. Delete it.",
            ))
        }
        [Segment::Key(head), Segment::Key(key), ..] if head == "tasks" && key == "eval" => {
            Some(Finding::warning(
                path.to_string(),
                "\"tasks.eval\" is no longer used: task evals were removed. Delete it.",
            ))
        }
        [Segment::Key(head), Segment::Key(key), ..] if head == "runtime" && key == "heartbeats" => {
            Some(Finding::warning(
                path.to_string(),
                "\"runtime.heartbeats\" is no longer used: scheduled heartbeat tasks were removed. Delete it.",
            ))
        }
        _ => None,
    }
}

pub const PLUGINS_IGNORED: &str = "\"plugins\" is ignored — the Rust runtime loads no plugins.";
pub const WORKFLOWS_IGNORED: &str = "\"workflows\" is ignored — workflows moved out of the runtime into the rivethub-workflows package.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Node,
    Standalone,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Presence {
    pub plugins: bool,
    pub workflows: bool,
}

pub fn validate(
    config: &Config,
    ignored: &[IgnoredPath],
    presence: &Presence,
    profile: Profile,
) -> ValidationResult {
    let mut out = ValidationResult::default();
    let ignored = retired::findings(ignored, &mut out);
    ignored_findings(config, &ignored, &mut out);
    if presence.plugins {
        out.warning("plugins", PLUGINS_IGNORED);
    }
    if presence.workflows {
        out.warning("workflows", WORKFLOWS_IGNORED);
    }
    sections(config, &mut out, profile);
    cross_references(config, &mut out);
    out
}

const fn operator_sections_required(profile: Profile) -> bool {
    match profile {
        Profile::Node => true,
        Profile::Standalone => false,
    }
}

fn sections(config: &Config, out: &mut ValidationResult, profile: Profile) {
    if let Some(section) = &config.runtime {
        runtime::validate(out, section);
    } else if operator_sections_required(profile) {
        out.error("runtime", "Missing required section \"runtime\"");
    }
    if let Some(agents) = &config.agents {
        agents::validate(out, agents);
    } else if operator_sections_required(profile) {
        out.error(
            "agents",
            "Missing required section \"agents\" — define at least one agent",
        );
    }
    if config.providers.is_none() && operator_sections_required(profile) {
        out.error(
            "providers",
            "Missing required section \"providers\" — define at least one provider",
        );
    }
    providers::validate(out, config.providers.as_ref());
    if let Some(channels) = &config.channels {
        channels::validate(out, channels);
    }
    if let Some(memory) = &config.memory {
        memory::validate(out, memory);
    }
    if let Some(section) = &config.deployment {
        deployment::validate(out, *section);
    }
    if let Some(section) = &config.gateway {
        gateway::validate(out, section);
    }
    if let Some(section) = &config.tasks {
        tasks::validate(out, section);
    }
    if let Some(entries) = &config.hubs {
        hubs::validate(out, entries);
    }
    if let Some(section) = &config.telemetry {
        hubs::telemetry(out, section);
    }
    if let Some(section) = &config.cloud {
        hubs::cloud(out, section);
    }
    if let Some(section) = &config.entities {
        entities::validate(out, section);
    }
}

fn ignored_findings(config: &Config, ignored: &[IgnoredPath], out: &mut ValidationResult) {
    for path in ignored {
        if let Some(message) = RemovedKey::from_path(&path.0).map(|removed| removed.to_string()) {
            out.error(path.to_string(), message);
        } else if let Some(finding) = removed_key_finding(config, path, &path.0) {
            out.push(finding);
        } else if let Some(message) = ignored_message(config, &path.0) {
            out.warning(path.to_string(), message);
        }
    }
}

fn ignored_message(config: &Config, segments: &[Segment]) -> Option<String> {
    if let [Segment::Key(key)] = segments {
        return top_level_message(key);
    }
    let [Segment::Key(head), rest @ ..] = segments else {
        return None;
    };
    let head: &str = head.as_str();
    match head {
        "runtime" => runtime_message(rest),
        "agents" => agents_message(rest),
        "providers" => providers_message(config, rest),
        "channels" => channels_message(rest),
        "memory" => memory_message(rest),
        "deployment" => deployment_message(rest),
        "gateway" => gateway_message(rest),
        "tasks" => tasks_message(rest),
        "entities" => entities_message(rest),
        _ => None,
    }
}

fn entities_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!("Unknown entities key \"{key}\"")),
        _ => None,
    }
}

fn runtime_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!("Unknown runtime key \"{key}\"")),
        _ => None,
    }
}

fn agents_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(_id), Segment::Key(key)] => Some(format!("Unknown agent key \"{key}\"")),
        [Segment::Key(_id), Segment::Key(parent), Segment::Key(key)] if parent == "tools" => Some(
            format!("Unknown tools filter key \"{key}\" — expected \"exclude\" or \"include\""),
        ),
        _ => None,
    }
}

fn providers_message(config: &Config, rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(id), Segment::Key(key)] => {
            let kind = config.provider_kind(id)?;
            Some(format!(
                "Unknown key \"{key}\" for provider type \"{kind}\""
            ))
        }
        _ => None,
    }
}

fn channels_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(id), Segment::Key(key)] if id == "agent" => {
            Some(format!("Unknown key \"{key}\" for channel type \"agent\""))
        }
        _ => None,
    }
}

fn memory_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!(
            "Unknown memory key \"{key}\" — supported: \"postgres\" / \"sqlite\" (backend), \"capture\" (write-path options)"
        )),
        [Segment::Key(parent), Segment::Key(key)] => {
            let parent: &str = parent.as_str();
            match parent {
                "postgres" => Some(format!("Unknown memory.postgres key \"{key}\"")),
                "sqlite" => Some(format!("Unknown memory.sqlite key \"{key}\"")),
                "capture" => Some(format!("Unknown memory.capture key \"{key}\"")),
                "models" => Some(format!(
                    "Unknown memory.models key \"{key}\" — supported: default, summarizer, categorizer, wiki_writer, embedder"
                )),
                _ => None,
            }
        }
        [Segment::Key(parent), Segment::Key(grand), Segment::Key(key)]
            if parent == "postgres" && grand == "embedded" =>
        {
            Some(format!("Unknown memory.postgres.embedded key \"{key}\""))
        }
        [Segment::Key(parent), Segment::Key(role), Segment::Key(key)] if parent == "models" => {
            Some(format!("Unknown memory.models.{role} key \"{key}\""))
        }
        [Segment::Key(parent), Segment::Key(grand), Segment::Key(key)]
            if parent == "capture" && grand == "redaction" =>
        {
            Some(format!("Unknown memory.capture.redaction key \"{key}\""))
        }
        _ => None,
    }
}

fn deployment_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!("Unknown deployment key \"{key}\"")),
        _ => None,
    }
}

fn gateway_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!("Unknown gateway key \"{key}\"")),
        _ => None,
    }
}

fn tasks_message(rest: &[Segment]) -> Option<String> {
    match rest {
        [Segment::Key(key)] => Some(format!("Unknown tasks key \"{key}\"")),
        [Segment::Key(parent), Segment::Key(_id), Segment::Key(key)] if parent == "harnesses" => {
            Some(format!("Unknown key \"{key}\""))
        }
        _ => None,
    }
}

fn top_level_message(path: &str) -> Option<String> {
    match path {
        "plugins" | "workflows" | "mcp" | "providers" => None,
        _ => Some(format!(
            "Unknown top-level key \"{path}\" — will be ignored"
        )),
    }
}

fn cross_references(config: &Config, out: &mut ValidationResult) {
    let agent_names: Vec<&str> = config
        .agents
        .iter()
        .flatten()
        .map(|(name, _)| name.as_str())
        .collect();
    let provider_names: Vec<&str> = config
        .providers
        .iter()
        .flatten()
        .map(|(name, _)| name.as_str())
        .collect();
    if let Some(runtime) = &config.runtime
        && let Some(default_agent) = &runtime.default_agent
        && !default_agent.is_empty()
        && !agent_names.contains(&default_agent.as_str())
    {
        out.error(
            "runtime.default_agent",
            format!(
                "Default agent \"{default_agent}\" is not defined in [agents]. Available: {}",
                available_list(&agent_names)
            ),
        );
    }
    if let Some(agents) = &config.agents {
        for (name, agent) in agents {
            agent_provider_cross_ref(out, name, agent, &provider_names);
        }
    }
    if let Some(memory) = &config.memory {
        memory::embed_provider_references(out, memory, config.providers.as_ref());
        if let Some(models) = &memory.models {
            memory::model_references(out, models, config.providers.as_ref());
        }
    }
}

fn agent_provider_cross_ref(
    out: &mut ValidationResult,
    name: &str,
    agent: &AgentEntry,
    provider_names: &[&str],
) {
    let Some(provider) = agent.provider.as_deref() else {
        return;
    };
    if provider_names.contains(&provider) {
        return;
    }
    out.error(
        format!("agents.{name}.provider"),
        format!(
            "Provider \"{provider}\" referenced by agent \"{name}\" is not defined in [providers]. Available: {}",
            available_list(provider_names)
        ),
    );
}

fn available_list(names: &[&str]) -> String {
    if names.is_empty() {
        "(none)".to_owned()
    } else {
        names.join(", ")
    }
}
