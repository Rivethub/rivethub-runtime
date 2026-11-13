use std::borrow::Cow;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::num::NonZeroU64;
use std::path::PathBuf;

use serde::Deserialize;

use crate::Environment;
use crate::enums::{
    DeploymentTarget, Effort, HarnessId, Isolation, MemoryBackend, ModelsMode, ThinkingLevel,
};
use crate::provider::{ProviderSlot, ProviderType};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(untagged)]
pub enum HarnessSlot {
    Known(HarnessId),
    Unknown(String),
}

impl HarnessSlot {
    pub fn as_text(&self) -> Cow<'_, str> {
        match self {
            Self::Known(id) => Cow::Owned(id.to_string()),
            Self::Unknown(id) => Cow::Borrowed(id),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runtime: Option<RuntimeSection>,
    pub agents: Option<BTreeMap<String, AgentEntry>>,
    #[serde(skip)]
    pub providers: Option<BTreeMap<String, ProviderSlot>>,
    pub channels: Option<BTreeMap<String, ChannelEntry>>,
    pub memory: Option<MemorySection>,
    pub transports: Option<BTreeMap<String, TransportSection>>,
    pub deployment: Option<DeploymentSection>,
    pub mesh: Option<MeshSection>,
    pub gateway: Option<GatewaySection>,
    pub tasks: Option<TasksSection>,
    pub hubs: Option<Vec<HubEntry>>,
    pub telemetry: Option<TelemetrySection>,
    pub cloud: Option<CloudSection>,
    pub entities: Option<EntitiesSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HubEntry {
    pub name: String,
    pub url: url::Url,
    #[serde(default)]
    pub kind: wire::gateway::account::HubKind,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TelemetrySection {
    pub disabled: bool,
    pub url: Option<url::Url>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CloudSection {
    pub url: Option<url::Url>,
    pub update_key: Option<String>,
    pub disabled: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct EntitiesSection {
    pub matcher: Option<String>,
    pub matcher_url: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RedactionSettings {
    pub enabled: bool,
    pub builtins: bool,
    pub patterns: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RuntimeSection {
    pub workspace: Option<PathBuf>,
    pub default_agent: Option<String>,
    pub turn_timeout: Option<u64>,
    pub context: Option<ContextSection>,
    pub skill_dirs: Option<Vec<PathBuf>>,
    pub plugin_dirs: Option<Vec<PathBuf>>,
    pub safety: Option<SafetySection>,
    pub auto_actions: Option<AutoActionsSection>,
    pub experimental: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ContextSection {
    pub soft_nudge_pct: Option<Vec<f64>>,
    pub hard_nudge_pct: Option<f64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct SafetySection {
    #[serde(default = "default_true", alias = "shellDanger")]
    pub shell_danger: bool,
    #[serde(default = "default_true")]
    pub audit: bool,
    #[serde(alias = "workspaceFence")]
    pub workspace_fence: Option<WorkspaceFenceSection>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct WorkspaceFenceSection {
    #[serde(alias = "allowedDirs")]
    pub allowed_dirs: Option<Vec<PathBuf>>,
    #[serde(alias = "alwaysAllow")]
    pub always_allow: Option<Vec<PathBuf>>,
    pub tools: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct AutoActionsSection {
    pub format: Option<bool>,
    pub lint: Option<bool>,
    pub test: Option<bool>,
    #[serde(alias = "gitCheck")]
    pub git_check: Option<bool>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct AgentEntry {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub default_thinking: Option<ThinkingLevel>,
    pub local: Option<bool>,
    pub tools: Option<ToolsSection>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ToolsSection {
    pub exclude: Option<Vec<String>>,
    pub include: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ChannelEntry {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub secret: Option<String>,
    #[serde(alias = "agentId")]
    pub agent_id: Option<String>,
    pub peers: Option<Vec<String>>,
    pub tls: Option<ChannelTls>,
    pub bot_token: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChannelTls {
    pub ca: Option<String>,
    pub cert: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct MemorySection {
    pub import_pg_dump: Option<PathBuf>,
    pub backend: Option<MemoryBackend>,
    pub postgres: Option<PostgresSection>,
    pub sqlite: Option<SqliteSection>,
    pub capture: CaptureSection,
    pub models: Option<crate::memory_models::MemoryModels>,
    pub sanitize: SanitizeSection,
    pub share: ShareSection,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ShareSection {
    pub server: Option<ShareServerSection>,
    pub trusted_cas: Vec<TrustedCa>,
    #[serde(alias = "allowedServers")]
    pub allowed_servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareServerSection {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedCa {
    pub host: String,
    pub ca: PathBuf,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct SanitizeSection {
    pub mapping: Option<PathBuf>,
    pub decide: Option<DecideSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideSection {
    pub url: String,
    #[serde(default = "default_decide_threshold")]
    pub threshold: f64,
    #[serde(default = "default_decide_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_decide_top")]
    pub top: usize,
    #[serde(default)]
    pub block: bool,
}

const fn default_decide_threshold() -> f64 {
    0.3
}

const fn default_decide_concurrency() -> usize {
    8
}

const fn default_decide_top() -> usize {
    20
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingFile {
    Configured(PathBuf),
    Default(PathBuf),
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct PostgresSection {
    pub connection_string: Option<String>,
    pub embed_endpoint: Option<String>,
    pub embed_model: Option<String>,
    pub embed_api_key: Option<String>,
    pub embed_provider: Option<String>,
    pub embed_expected_dims: Option<u32>,
    pub embed_query_instruction: Option<String>,
    pub embed_timeout_ms: Option<u64>,
    pub chars_per_chunk: Option<u32>,
    pub hnsw_ef_search: Option<u64>,
    pub delegation_tracking: bool,
    pub embedded: Option<EmbeddedSection>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct EmbeddedSection {
    pub data_dir: Option<PathBuf>,
    #[serde(default = "default_embedded_port")]
    pub port: u16,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_embedded_port() -> u16 {
    5433
}

fn default_max_connections() -> u32 {
    96
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct SqliteSection {
    pub path: Option<PathBuf>,
    pub embed_endpoint: Option<String>,
    pub embed_model: Option<String>,
    pub embed_api_key: Option<String>,
    pub embed_provider: Option<String>,
    pub embed_query_instruction: Option<String>,
    pub tagging: Option<bool>,
    pub workers: Option<bool>,
    pub wiki_dir: Option<PathBuf>,
    pub project_rule: Option<String>,
    pub tagger_allow_protected_removals: Option<bool>,
    pub tagger_api_key: Option<String>,
    pub tagger_token_command: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct CaptureSection {
    pub redaction: RedactionSection,
    pub gateway: Option<CaptureGateway>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaptureGateway {
    pub url: String,
    pub ca: PathBuf,
    pub token: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct RedactionSection {
    pub enabled: Option<bool>,
    pub builtins: Option<bool>,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MeshSection {
    pub node_name: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct GatewaySection {
    pub enabled: Option<bool>,
    pub host: Option<String>,
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    pub token: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub users_file: Option<PathBuf>,
    pub static_dir: Option<PathBuf>,
    pub root_redirect: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub request_deadline_secs: Option<NonZeroU64>,
    pub fresh_login_secs: Option<u64>,
    pub trusted_proxies: Vec<TrustedProxy>,
    pub drops: DropsSection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct TrustedProxy(ipnet::IpNet);

impl TrustedProxy {
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        self.0.contains(&address.to_canonical())
    }
}

impl TryFrom<String> for TrustedProxy {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let text = text.trim();
        if let Ok(net) = text.parse::<ipnet::IpNet>() {
            return Ok(Self(unmapped(net)));
        }
        text.parse::<IpAddr>()
            .map(|address| Self(ipnet::IpNet::from(address.to_canonical())))
            .map_err(|_| format!("{text:?} is not an IP address or CIDR block"))
    }
}

fn unmapped(net: ipnet::IpNet) -> ipnet::IpNet {
    let ipnet::IpNet::V6(v6) = net else {
        return net;
    };
    match (
        v6.network().to_ipv4_mapped(),
        v6.prefix_len().checked_sub(96),
    ) {
        (Some(network), Some(prefix)) => {
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(
                    fallback,
                    reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract reads a mapped block as its IPv4 block; a v6 prefix of at most 128 leaves an IPv4 prefix of at most 32, so the entry is never kept as written"
                )
            )]
            let unmapped = ipnet::Ipv4Net::new(network, prefix).map_or(net, ipnet::IpNet::V4);
            unmapped
        }
        _ => net,
    }
}

fn default_gateway_port() -> u16 {
    5174
}

pub const DROP_MAX_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DropsSection {
    pub enabled: bool,
    pub root: Option<PathBuf>,
    pub max_bytes: u64,
    pub per_user_drops: u32,
    pub per_user_bytes: u64,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 143: the drop box defaults are the rivethub.cloud limits, not the derive's zeros"
    )
)]
impl Default for DropsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            root: None,
            max_bytes: DROP_MAX_BYTES,
            per_user_drops: 50,
            per_user_bytes: 512 << 20,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TasksSection {
    pub harnesses: Option<BTreeMap<HarnessSlot, HarnessEntry>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct HarnessEntry {
    pub binary: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub cwd: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub models: Option<Vec<String>>,
    pub efforts: Option<Vec<Effort>>,
    pub models_mode: Option<ModelsMode>,
    pub isolation: Option<Isolation>,
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct TransportSection {
    pub host: Option<String>,
    #[serde(default = "default_transport_port")]
    pub port: u16,
    pub socket: Option<PathBuf>,
    pub auth_token: Option<String>,
    pub require_bearer_on_socket: Option<bool>,
}

fn default_transport_port() -> u16 {
    5700
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct DeploymentSection {
    pub target: Option<DeploymentTarget>,
}

impl Config {
    #[must_use]
    pub fn memory_postgres_url(&self, env: &Environment) -> Option<String> {
        self.memory
            .as_ref()
            .and_then(|memory| memory.postgres.as_ref())
            .and_then(|postgres| postgres.connection_string.as_deref())
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .or_else(|| env.postgres_url())
    }

    #[must_use]
    pub fn memory_embed_endpoint(&self, env: &Environment) -> Option<String> {
        self.postgres_memory_text(|postgres| postgres.embed_endpoint.as_deref())
            .or_else(|| self.embed_provider_url(MemoryBackend::Postgres))
            .or_else(|| env.embed_url())
    }

    #[must_use]
    pub fn memory_embed_model(&self, env: &Environment) -> Option<String> {
        self.postgres_memory_text(|postgres| postgres.embed_model.as_deref())
            .or_else(|| env.embed_model())
    }

    fn postgres_memory_text(
        &self,
        field: impl Fn(&PostgresSection) -> Option<&str>,
    ) -> Option<String> {
        self.memory
            .as_ref()
            .and_then(|memory| memory.postgres.as_ref())
            .and_then(field)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    #[must_use]
    pub fn sqlite_embed_endpoint(&self, env: &Environment) -> Option<String> {
        self.sqlite_memory_text(|sqlite| sqlite.embed_endpoint.as_deref())
            .or_else(|| self.embed_provider_url(MemoryBackend::Sqlite))
            .or_else(|| env.embed_url())
    }

    #[must_use]
    pub fn sqlite_embed_model(&self, env: &Environment) -> Option<String> {
        self.sqlite_memory_text(|sqlite| sqlite.embed_model.as_deref())
            .or_else(|| env.embed_model())
    }

    fn sqlite_memory_text(&self, field: impl Fn(&SqliteSection) -> Option<&str>) -> Option<String> {
        self.memory
            .as_ref()
            .and_then(|memory| memory.sqlite.as_ref())
            .and_then(field)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    }

    #[must_use]
    pub fn postgres_selected(&self, env: &Environment) -> bool {
        !self.sqlite_selected() && self.memory_postgres_url(env).is_some()
    }

    #[must_use]
    pub fn sqlite_selected(&self) -> bool {
        self.memory.as_ref().is_some_and(|memory| {
            memory.backend == Some(MemoryBackend::Sqlite)
                || (memory.backend.is_none()
                    && memory.sqlite.is_some()
                    && memory.postgres.is_none())
        }) || self.pglite_import_pending()
    }

    pub fn pglite_import_pending(&self) -> bool {
        self.memory
            .as_ref()
            .and_then(|memory| memory.postgres.as_ref())
            .is_some_and(|postgres| postgres.embedded.is_some())
    }

    pub fn capture_redaction(&self) -> RedactionSettings {
        let redaction = self
            .memory
            .as_ref()
            .map_or_else(RedactionSection::default, |memory| {
                memory.capture.redaction.clone()
            });
        RedactionSettings {
            enabled: redaction.enabled == Some(true),
            builtins: redaction.builtins != Some(false),
            patterns: redaction.patterns,
        }
    }

    pub fn sanitize_mapping(&self, env: &Environment) -> Result<MappingFile, crate::ConfigError> {
        let configured = self
            .memory
            .as_ref()
            .and_then(|memory| memory.sanitize.mapping.as_ref());
        match configured {
            Some(path) => Ok(MappingFile::Configured(
                crate::paths::expand_home(env, path)?.unwrap_or_else(|| path.clone()),
            )),
            None => Ok(MappingFile::Default(
                crate::Paths::new(env)?
                    .home()
                    .join(".config")
                    .join("rivethub")
                    .join("redactions.toml"),
            )),
        }
    }

    #[must_use]
    pub fn sanitize_decide(&self) -> Option<&DecideSection> {
        self.memory.as_ref()?.sanitize.decide.as_ref()
    }

    pub fn share_server(&self) -> Option<&ShareServerSection> {
        self.memory.as_ref()?.share.server.as_ref()
    }

    #[must_use]
    pub fn cloud_disabled(&self) -> bool {
        self.cloud.as_ref().is_some_and(|section| section.disabled)
    }

    #[must_use]
    pub fn telemetry_disabled(&self) -> bool {
        self.telemetry
            .as_ref()
            .is_some_and(|section| section.disabled)
            || (self.cloud_disabled() && self.telemetry_url().is_none())
    }

    #[must_use]
    pub fn telemetry_url(&self) -> Option<&url::Url> {
        self.telemetry
            .as_ref()
            .and_then(|section| section.url.as_ref())
    }

    #[must_use]
    pub fn share_allowed_servers(&self) -> &[String] {
        self.memory
            .as_ref()
            .map_or(&[], |memory| memory.share.allowed_servers.as_slice())
    }

    pub fn share_trusted_cas(&self) -> &[TrustedCa] {
        self.memory
            .as_ref()
            .map_or(&[], |memory| memory.share.trusted_cas.as_slice())
    }

    pub fn provider_kind(&self, name: &str) -> Option<ProviderType> {
        self.providers
            .as_ref()?
            .get(name)
            .and_then(|slot| slot.kind)
    }
}
