use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use strum::IntoEnumIterator as _;

use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::enums::MemoryBackend;
use crate::environment::{CLOUD_VARIABLE, Environment};
use crate::model::Config;
use crate::paths::{ConfigLocation, Paths, expand_home};
use crate::provider::{
    CliProvider, OpenaiProvider, ProviderBody, ProviderSlot, ProviderType, RemovedProviderType,
    UnknownProvider,
};
use crate::report::{Finding, ValidationResult};
use crate::resolve::resolve_env_vars_with;
use crate::validate::{self, IgnoredPath};

const GATEWAY_SECTION: &str = "gateway";
const LEGACY_GATEWAY_SECTION: &str = "den";
const LEGACY_GATEWAY_NOTICE: &str =
    "den is deprecated — rename the config section to gateway; the settings are unchanged.";
const BOTH_GATEWAY_SECTIONS: &str = "den and gateway are both set — den is the deprecated name of the gateway section; keep only gateway.";

const PGLITE_IMPORT_NOTICE: &str = "memory.postgres.embedded is set — the embedded Postgres (PGlite) option was removed; the memory backend resolves to SQLite and the one-time import runs at first start.";

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<Finding>,
    pub notices: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("\"{path}\" has the wrong type: {message}")]
    Type { path: String, message: String },
    #[error("Config validation failed:\n{result}")]
    Validation { result: ValidationResult },
    #[error("HOME must be an absolute directory path without '..' components for RivetHub state")]
    Home,
    #[error("RIVETHUB_HOME must be an absolute directory path without '..' components")]
    StateHome,
    #[error(
        "a node name is required: set mesh.node_name or RIVETHUB_NODE_NAME, or provide a hostname"
    )]
    NodeName,
    #[error("environment substitution pattern is invalid: {0}")]
    Substitution(#[from] regress::Error),
    #[error("environment variable {name} must contain valid UTF-8 text")]
    EnvironmentText { name: String },
    #[error("{CLOUD_VARIABLE} must be on or off, not {0:?}")]
    CloudSwitch(String),
}

#[derive(Debug)]
pub struct Typed {
    pub config: Config,
    pub ignored: Vec<IgnoredPath>,
}

pub fn resolve_config_path(env: &Environment) -> Result<Option<ConfigLocation>, ConfigError> {
    let paths = Paths::new(env)?;
    if let Some(path) = env.get(crate::CONFIG_PATH_VARIABLE) {
        return Ok(Some(ConfigLocation::Current(PathBuf::from(path))));
    }
    for location in paths.config_candidates() {
        match std::fs::metadata(location.path()) {
            Ok(metadata) if metadata.is_file() => return Ok(Some(location)),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigError::Io {
                    path: location.path().to_owned(),
                    source,
                });
            }
        }
    }
    Ok(None)
}

pub fn validate_document(
    document: &serde_yaml_ng::Value,
    profile: validate::Profile,
) -> ValidationResult {
    let document = match gateway_section(document) {
        Ok(document) => document,
        Err((path, message)) => {
            let mut out = ValidationResult::default();
            out.error(&path, message);
            return out;
        }
    };
    let presence = validate::Presence {
        plugins: has_key(&document, "plugins"),
        workflows: has_key(&document, "workflows"),
    };
    match typed_document(&document) {
        Ok(typed) => validate::validate(&typed.config, &typed.ignored, &presence, profile),
        Err((path, message)) => {
            let mut out = ValidationResult::default();
            out.error(&path, format!("\"{path}\" has the wrong type: {message}"));
            out
        }
    }
}

pub fn type_document(document: &serde_yaml_ng::Value) -> Result<Typed, ConfigError> {
    typed_document(document).map_err(|(path, message)| ConfigError::Type { path, message })
}

fn typed_document(document: &serde_yaml_ng::Value) -> Result<Typed, (String, String)> {
    let document = gateway_section(document)?;
    let mut ignored = Vec::new();
    let mut config = deserialize_main(&document, &mut ignored)?;
    config.providers = provider_map(&document, &mut ignored)?;
    Ok(Typed { config, ignored })
}

fn gateway_section(
    document: &serde_yaml_ng::Value,
) -> Result<Cow<'_, serde_yaml_ng::Value>, (String, String)> {
    let Some(mapping) = document
        .as_mapping()
        .filter(|mapping| mapping.contains_key(LEGACY_GATEWAY_SECTION))
    else {
        return Ok(Cow::Borrowed(document));
    };
    if mapping.contains_key(GATEWAY_SECTION) {
        return Err((
            LEGACY_GATEWAY_SECTION.to_owned(),
            BOTH_GATEWAY_SECTIONS.to_owned(),
        ));
    }
    let renamed = mapping
        .iter()
        .map(|(key, value)| {
            let key = if key.as_str() == Some(LEGACY_GATEWAY_SECTION) {
                serde_yaml_ng::Value::String(GATEWAY_SECTION.to_owned())
            } else {
                key.clone()
            };
            (key, value.clone())
        })
        .collect();
    Ok(Cow::Owned(serde_yaml_ng::Value::Mapping(renamed)))
}

fn has_key(document: &serde_yaml_ng::Value, key: &str) -> bool {
    document.get(key).is_some()
}

fn deserialize_main(
    document: &serde_yaml_ng::Value,
    ignored: &mut Vec<IgnoredPath>,
) -> Result<Config, (String, String)> {
    if matches!(document, serde_yaml_ng::Value::Null) {
        return Err((
            String::new(),
            "invalid type: null, expected struct Config".to_owned(),
        ));
    }
    tracked::<Config>(document, "", ignored)
}

fn tracked<T: DeserializeOwned>(
    raw: &serde_yaml_ng::Value,
    prefix: &str,
    ignored: &mut Vec<IgnoredPath>,
) -> Result<T, (String, String)> {
    let mut collect =
        |path: serde_ignored::Path<'_>| ignored.push(IgnoredPath::collect(prefix, &path));
    serde_path_to_error::deserialize(serde_ignored::Deserializer::new(raw, &mut collect))
        .map_err(|error| conflict_at(&error, prefix))
}

fn provider_map(
    document: &serde_yaml_ng::Value,
    ignored: &mut Vec<IgnoredPath>,
) -> Result<Option<BTreeMap<String, ProviderSlot>>, (String, String)> {
    let Some(entries) = document.get("providers") else {
        return Ok(None);
    };
    if entries.is_null() {
        return Ok(None);
    }
    let entries: BTreeMap<String, serde_yaml_ng::Value> = serde_path_to_error::deserialize(entries)
        .map_err(|error| conflict_at(&error, "providers"))?;
    let mut map = BTreeMap::new();
    for (id, raw) in &entries {
        let prefix = format!("providers.{id}");
        let declared = declared_type(raw, &prefix)?;
        let name = declared.as_deref().map_or(id.as_str(), |name| name);
        let kind = ProviderType::iter().find(|kind| kind.to_string() == name);
        let body = match kind {
            Some(kind) if kind.is_openai_family() => {
                ProviderBody::Openai(Box::new(tracked::<OpenaiProvider>(raw, &prefix, ignored)?))
            }
            Some(_) => ProviderBody::Cli(Box::new(tracked::<CliProvider>(raw, &prefix, ignored)?)),
            None => match RemovedProviderType::iter().find(|kind| kind.to_string() == name) {
                Some(removed) => ProviderBody::Removed(removed),
                None => ProviderBody::Unknown(tracked::<UnknownProvider>(raw, &prefix, ignored)?),
            },
        };
        map.insert(id.clone(), ProviderSlot { kind, body });
    }
    Ok(Some(map))
}

#[derive(Debug, Default, Deserialize)]
struct DeclaredType {
    #[serde(default, rename = "type")]
    declared: Option<String>,
}

fn declared_type(
    raw: &serde_yaml_ng::Value,
    prefix: &str,
) -> Result<Option<String>, (String, String)> {
    serde_path_to_error::deserialize::<_, DeclaredType>(raw)
        .map(|tag| tag.declared)
        .map_err(|error| conflict_at(&error, prefix))
}

fn conflict_at<E: std::fmt::Display>(
    error: &serde_path_to_error::Error<E>,
    prefix: &str,
) -> (String, String) {
    let relative = error.path().to_string();
    let path = if relative.is_empty() || relative == "." {
        prefix.to_owned()
    } else if prefix.is_empty() {
        relative
    } else {
        format!("{prefix}.{relative}")
    };
    (path, error.inner().to_string())
}

impl Config {
    pub fn load(
        env: &Environment,
        location: &ConfigLocation,
        profile: validate::Profile,
    ) -> Result<Loaded, ConfigError> {
        let path = location.path();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut loaded = Self::parse(env, &text, profile)?;
        if location.is_legacy() {
            loaded.notices.insert(
                0,
                format!(
                    "Using legacy config path {} — move it to {} so both runtimes agree.",
                    path.display(),
                    Paths::new(env)?.hub_home().display()
                ),
            );
        }
        Ok(loaded)
    }

    pub fn from_str(env: &Environment, text: &str) -> Result<Loaded, ConfigError> {
        Self::parse(env, text, validate::Profile::Node)
    }

    fn parse(
        env: &Environment,
        text: &str,
        profile: validate::Profile,
    ) -> Result<Loaded, ConfigError> {
        let mut document: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
        let result = validate_document(&document, profile);
        if !result.valid() {
            return Err(ConfigError::Validation { result });
        }
        resolve_env_vars_with(env, &mut document)?;
        let resolved = validate_document(&document, profile);
        if !resolved.valid() {
            return Err(ConfigError::Validation {
                result: ValidationResult {
                    warnings: result.warnings,
                    ..resolved
                },
            });
        }
        let mut typed = type_document(&document)?;
        expand_workspace(env, &mut typed.config)?;
        let notices = notices(env, &document, &typed.config);
        Ok(Loaded {
            config: typed.config,
            warnings: result.warnings,
            notices,
        })
    }
}

fn expand_workspace(env: &Environment, config: &mut Config) -> Result<(), ConfigError> {
    if let Some(workspace) = config
        .runtime
        .as_mut()
        .and_then(|runtime| runtime.workspace.as_mut())
        && let Some(expanded) = expand_home(env, workspace)?
    {
        *workspace = expanded;
    }
    Ok(())
}

fn notices(env: &Environment, document: &serde_yaml_ng::Value, config: &Config) -> Vec<String> {
    let mut out = env.legacy_notices();
    out.extend(env.deprecation_warnings());
    out.extend(plugin_entry_notices(document));
    if has_key(document, LEGACY_GATEWAY_SECTION) {
        out.push(LEGACY_GATEWAY_NOTICE.to_owned());
    }
    if config.pglite_import_pending() {
        out.push(PGLITE_IMPORT_NOTICE.to_owned());
    }
    out
}

pub fn standalone_config_yaml(
    database: &std::path::Path,
    host: &str,
    port: u16,
    cloud_disabled: bool,
) -> Result<String, ConfigError> {
    let document = StandaloneDocument {
        memory: StandaloneMemory {
            backend: MemoryBackend::Sqlite,
            sqlite: StandaloneSqlite { path: database },
        },
        gateway: StandaloneGateway { host, port },
        cloud: cloud_disabled.then_some(StandaloneCloud { disabled: true }),
    };
    Ok(serde_yaml_ng::to_string(&document)?)
}

#[derive(Serialize)]
struct StandaloneDocument<'a> {
    memory: StandaloneMemory<'a>,
    gateway: StandaloneGateway<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloud: Option<StandaloneCloud>,
}

#[derive(Serialize)]
struct StandaloneCloud {
    disabled: bool,
}

#[derive(Serialize)]
struct StandaloneMemory<'a> {
    backend: MemoryBackend,
    sqlite: StandaloneSqlite<'a>,
}

#[derive(Serialize)]
struct StandaloneSqlite<'a> {
    path: &'a std::path::Path,
}

#[derive(Serialize)]
struct StandaloneGateway<'a> {
    host: &'a str,
    port: u16,
}

fn plugin_entry_notices(document: &serde_yaml_ng::Value) -> Vec<String> {
    document
        .get("plugins")
        .and_then(serde_yaml_ng::Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(serde_yaml_ng::Value::as_str)
        .filter(|entry| entry.starts_with("plugins/tools/"))
        .map(|entry| {
            format!(
                "{entry} is deprecated — built-in tools were removed; every harness ships its own."
            )
        })
        .collect()
}
