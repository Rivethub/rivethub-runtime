use std::collections::BTreeMap;
use std::sync::LazyLock;

use regress::Regex;

use crate::memory_models::{EmbedderChoice, MemoryModels, ModelChoice};
use crate::model::{
    EmbeddedSection, MemorySection, PostgresSection, RedactionSection, SqliteSection,
};
use crate::provider::{ProviderBody, ProviderSlot};
use crate::report::ValidationResult;

use super::looks_like_api_key;

const NESTED_QUANTIFIER_PATTERN: &str =
    r"\((?:[^\\)]|\\.)*[+*](?:[^\\)]|\\.)*\)(?:[+*?]|\{\d+,?\d*\})";

static NESTED_QUANTIFIERS: LazyLock<Result<Regex, regress::Error>> =
    LazyLock::new(|| Regex::new(NESTED_QUANTIFIER_PATTERN));

pub(super) fn validate(out: &mut ValidationResult, memory: &MemorySection) {
    if memory
        .recall
        .own_weight
        .is_some_and(|weight| !weight.is_finite() || !(1.0..=2.0).contains(&weight))
    {
        out.error(
            "memory.recall.own_weight",
            "must be finite and between 1.0 and 2.0",
        );
    }
    if memory.postgres.is_some() && memory.sqlite.is_some() {
        out.error(
            "memory",
            "\"postgres\" and \"sqlite\" cannot both be set — pick one memory backend",
        );
    }
    if let Some(postgres) = &memory.postgres {
        postgres_section(out, postgres);
    }
    if let Some(sqlite) = &memory.sqlite {
        sqlite_section(out, sqlite);
    }
    redaction_section(out, &memory.capture.redaction);
}

fn postgres_section(out: &mut ValidationResult, postgres: &PostgresSection) {
    if postgres.embedded.is_some() && postgres.connection_string.is_some() {
        out.error(
            "memory.postgres",
            "\"embedded\" and \"connection_string\" cannot both be set — embedded owns the loopback URL",
        );
    }
    embed_api_key(postgres, out);
    if postgres
        .embed_expected_dims
        .is_some_and(|dims| dims != 1024)
    {
        out.error(
            "memory.postgres.embed_expected_dims",
            "\"embed_expected_dims\" must equal the embedding column width (1024); any other value bricks halfvec inserts and vector search",
        );
    }
    if postgres.chars_per_chunk == Some(0) {
        out.error(
            "memory.postgres.chars_per_chunk",
            "\"chars_per_chunk\" must be a positive integer",
        );
    }
    if let Some(embedded) = &postgres.embedded {
        embedded_section(out, embedded);
    }
}

fn embed_api_key(postgres: &PostgresSection, out: &mut ValidationResult) {
    let Some(key) = &postgres.embed_api_key else {
        return;
    };
    if key.contains("${") || key.is_empty() || !looks_like_api_key(key) {
        return;
    }
    out.warning(
        "memory.postgres.embed_api_key",
        "memory.postgres.embed_api_key appears hardcoded — prefer environment variables",
    );
}

pub(super) fn embed_provider_references(
    out: &mut ValidationResult,
    memory: &MemorySection,
    providers: Option<&BTreeMap<String, ProviderSlot>>,
) {
    if let Some(postgres) = &memory.postgres
        && let Some(id) = &postgres.embed_provider
    {
        let section = EmbedSection {
            path: "memory.postgres.embed_provider",
            endpoint: postgres.embed_endpoint.is_some(),
            api_key: postgres.embed_api_key.is_some(),
        };
        embed_provider(out, &section, id, providers);
    }
    if let Some(sqlite) = &memory.sqlite
        && let Some(id) = &sqlite.embed_provider
    {
        let section = EmbedSection {
            path: "memory.sqlite.embed_provider",
            endpoint: sqlite.embed_endpoint.is_some(),
            api_key: sqlite.embed_api_key.is_some(),
        };
        embed_provider(out, &section, id, providers);
    }
}

pub(super) fn model_references(
    out: &mut ValidationResult,
    models: &MemoryModels,
    providers: Option<&BTreeMap<String, ProviderSlot>>,
) {
    chat_choice(out, "memory.models.default", &models.default, providers);
    let optional = [
        ("memory.models.summarizer", &models.summarizer),
        ("memory.models.categorizer", &models.categorizer),
        ("memory.models.wiki_writer", &models.wiki_writer),
    ];
    for (path, choice) in optional {
        if let Some(choice) = choice {
            chat_choice(out, path, choice, providers);
        }
    }
    if let Some(embedder) = &models.embedder {
        embedder_choice(out, embedder, providers);
    }
}

fn provider_slot<'a>(
    out: &mut ValidationResult,
    path: &str,
    id: &str,
    providers: Option<&'a BTreeMap<String, ProviderSlot>>,
) -> Option<&'a ProviderSlot> {
    if id.trim().is_empty() {
        out.error(
            path,
            format!("\"{path}.provider\" must be a non-empty string"),
        );
        return None;
    }
    let slot = providers.and_then(|providers| providers.get(id));
    if slot.is_none() {
        let names: Vec<&str> = providers
            .into_iter()
            .flatten()
            .map(|(name, _)| name.as_str())
            .collect();
        out.error(
            path,
            format!(
                "Provider \"{id}\" referenced by {path} is not defined in [providers]. Available: {}",
                super::available_list(&names)
            ),
        );
    }
    slot
}

fn chat_choice(
    out: &mut ValidationResult,
    path: &str,
    choice: &ModelChoice,
    providers: Option<&BTreeMap<String, ProviderSlot>>,
) {
    let Some(slot) = provider_slot(out, path, &choice.provider, providers) else {
        return;
    };
    let named = choice
        .model
        .as_deref()
        .is_some_and(|model| !model.trim().is_empty());
    match &slot.body {
        ProviderBody::Cli(body) => {
            let configured = body.model.as_deref().is_some_and(|model| !model.is_empty());
            if !named && !configured {
                out.error(
                    format!("{path}.model"),
                    format!(
                        "\"{path}.model\" is required: provider \"{}\" has no model of its own",
                        choice.provider
                    ),
                );
            }
        }
        ProviderBody::Openai(body) => {
            if !named && body.model.is_none() {
                out.error(
                    format!("{path}.model"),
                    format!(
                        "\"{path}.model\" is required: provider \"{}\" has no model of its own",
                        choice.provider
                    ),
                );
            }
            if choice.effort.is_some() {
                out.warning(
                    format!("{path}.effort"),
                    format!(
                        "\"{path}.effort\" is ignored: provider \"{}\" is not a CLI provider",
                        choice.provider
                    ),
                );
            }
        }
        ProviderBody::Unknown(_) | ProviderBody::Removed(_) => out.error(
            path,
            format!(
                "Provider \"{}\" referenced by {path} has no supported type",
                choice.provider
            ),
        ),
    }
}

fn embedder_choice(
    out: &mut ValidationResult,
    choice: &EmbedderChoice,
    providers: Option<&BTreeMap<String, ProviderSlot>>,
) {
    let path = "memory.models.embedder";
    let endpoint = choice
        .endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.trim().is_empty());
    match (&choice.provider, endpoint) {
        (Some(_), Some(_)) => out.error(
            path,
            "\"provider\" and \"endpoint\" cannot both be set — the provider's base_url is the embeddings endpoint",
        ),
        (None, None) => out.error(path, "set \"provider\" or \"endpoint\" for the embedder"),
        (Some(id), None) => {
            if let Some(slot) = provider_slot(out, path, id, providers) {
                match &slot.body {
                    ProviderBody::Openai(body) => {
                        if choice.model.is_none() && body.model.is_none() {
                            out.error(
                                format!("{path}.model"),
                                format!("\"{path}.model\" is required: provider \"{id}\" has no model of its own"),
                            );
                        }
                    }
                    ProviderBody::Cli(_) | ProviderBody::Unknown(_) | ProviderBody::Removed(_) => {
                        out.error(
                            path,
                            format!(
                                "Provider \"{id}\" referenced by {path} cannot serve embeddings — use an openai-compatible or vllm provider"
                            ),
                        );
                    }
                }
            }
        }
        (None, Some(endpoint)) => {
            if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
                out.error(
                    format!("{path}.endpoint"),
                    format!("\"{path}.endpoint\" must be an http:// or https:// URL"),
                );
            }
            if choice.model.as_deref().is_none_or(|model| model.trim().is_empty()) {
                out.error(
                    format!("{path}.model"),
                    format!("\"{path}.model\" is required with an endpoint"),
                );
            }
        }
    }
}

struct EmbedSection {
    path: &'static str,
    endpoint: bool,
    api_key: bool,
}

fn embed_provider(
    out: &mut ValidationResult,
    section: &EmbedSection,
    id: &str,
    providers: Option<&BTreeMap<String, ProviderSlot>>,
) {
    let path = section.path;
    if section.endpoint {
        out.error(
            path,
            "\"embed_provider\" and \"embed_endpoint\" cannot both be set — the provider's base_url is the embeddings endpoint",
        );
    }
    if section.api_key {
        out.error(
            path,
            "\"embed_provider\" and \"embed_api_key\" cannot both be set — the provider's api_key or token_command authenticates embeddings",
        );
    }
    let Some(slot) = providers.and_then(|providers| providers.get(id)) else {
        let names: Vec<&str> = providers
            .into_iter()
            .flatten()
            .map(|(name, _)| name.as_str())
            .collect();
        out.error(
            path,
            format!(
                "Provider \"{id}\" referenced by {path} is not defined in [providers]. Available: {}",
                super::available_list(&names)
            ),
        );
        return;
    };
    match &slot.body {
        ProviderBody::Openai(_) => {}
        ProviderBody::Cli(_) | ProviderBody::Unknown(_) | ProviderBody::Removed(_) => out.error(
            path,
            format!(
                "Provider \"{id}\" referenced by {path} cannot serve embeddings — use an openai-compatible or vllm provider"
            ),
        ),
    }
}

fn embedded_section(out: &mut ValidationResult, embedded: &EmbeddedSection) {
    if embedded
        .data_dir
        .as_ref()
        .is_some_and(|dir| dir.as_os_str().is_empty())
    {
        out.error(
            "memory.postgres.embedded.data_dir",
            "\"data_dir\" must be a non-empty string",
        );
    }
    if embedded.port == 0 {
        out.error(
            "memory.postgres.embedded.port",
            "\"port\" must be an integer between 1 and 65535",
        );
    }
    if embedded.max_connections == 0 {
        out.error(
            "memory.postgres.embedded.max_connections",
            "\"max_connections\" must be a positive integer",
        );
    }
}

fn sqlite_section(out: &mut ValidationResult, sqlite: &SqliteSection) {
    match sqlite.path.as_deref() {
        Some(path) if path.as_os_str().is_empty() => out.error(
            "memory.sqlite.path",
            "\"memory.sqlite.path\" must be a non-empty file path",
        ),
        None => out.error(
            "memory.sqlite.path",
            "\"memory.sqlite.path\" is required (file path or \":memory:\")",
        ),
        Some(_) => {}
    }
}

fn redaction_section(out: &mut ValidationResult, redaction: &RedactionSection) {
    for (index, pattern) in redaction.patterns.iter().enumerate() {
        pattern_finding(out, index, pattern);
    }
}

fn pattern_finding(out: &mut ValidationResult, index: usize, pattern: &str) {
    let path = format!("memory.capture.redaction.patterns[{index}]");
    if pattern.is_empty() {
        out.error(
            path,
            "Each pattern must be a non-empty string (JS regex source)",
        );
        return;
    }
    let nested = match NESTED_QUANTIFIERS.as_ref() {
        Ok(nested) => nested,
        Err(error) => {
            out.error(path, format!("Cannot validate nested quantifiers: {error}"));
            return;
        }
    };
    if nested.find(pattern).is_some() {
        out.error(
            path,
            concat!(
                "Pattern looks ReDoS-prone (nested quantifiers); rewrite without nested +/",
                "* groups"
            ),
        );
        return;
    }
    if let Err(reason) = Regex::new(pattern) {
        out.error(path, format!("Invalid regex: {reason}"));
    }
}
