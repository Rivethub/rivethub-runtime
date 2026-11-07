use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::enums::LogLevel;

#[derive(Debug, Clone, Default)]
pub struct Environment {
    vars: BTreeMap<OsString, OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLevelSetting {
    pub level: LogLevel,
    pub ignored: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermMuxSetting {
    Auto,
    Herdr,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermMuxChoice {
    pub setting: TermMuxSetting,
    pub ignored: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceEndpoints {
    pub stt_url: Option<String>,
    pub tts_url: Option<String>,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermSettings {
    pub max_ptys: usize,
    pub scrollback_bytes: usize,
    pub detached_ttl_ms: u64,
    pub idle_ttl_ms: u64,
    pub exit_linger_ms: u64,
    pub session_gc_ms: u64,
    pub ignored: Vec<String>,
}

impl TermSettings {
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            max_ptys: 4,
            scrollback_bytes: 262_144,
            detached_ttl_ms: 1_800_000,
            idle_ttl_ms: 1_800_000,
            exit_linger_ms: 60_000,
            session_gc_ms: 0,
            ignored: Vec::new(),
        }
    }
}

pub const CONFIG_PATH_VARIABLE: &str = "RIVETHUB_CONFIG";
pub const LOG_LEVEL_VARIABLE: &str = "RIVETHUB_LOG_LEVEL";
pub const NODE_NAME_VARIABLE: &str = "RIVETHUB_NODE_NAME";
pub const NODE_ID_VARIABLE: &str = "RIVETHUB_NODE_ID";
pub const MODEL_RUN_VARIABLE: &str = "RIVETHUB_MODEL_RUN";
pub const POSTGRES_URL_VARIABLE: &str = "RIVETHUB_PG_URL";
pub const UPLOAD_MAX_BYTES_VARIABLE: &str = "RIVETHUB_UPLOAD_MAX_BYTES";
pub const UPLOAD_TTL_MS_VARIABLE: &str = "RIVETHUB_UPLOAD_TTL_MS";
pub const DEFAULT_UPLOAD_MAX_BYTES: u64 = 25 * 1024 * 1024;
pub const DEFAULT_UPLOAD_TTL_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSettings {
    pub max_bytes: u64,
    pub ttl: std::time::Duration,
    pub ignored: Vec<String>,
}

impl UploadSettings {
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            max_bytes: DEFAULT_UPLOAD_MAX_BYTES,
            ttl: std::time::Duration::from_millis(DEFAULT_UPLOAD_TTL_MS),
            ignored: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alias {
    LogLevel,
    NodeName,
    NodeId,
    PostgresUrl,
    EmbedUrl,
    EmbedModel,
    Host,
    UsersFile,
    SharedDir,
    InstallRoot,
    TermMux,
    TermMax,
    TermScrollback,
    TermDetachedTtl,
    TermIdleTtl,
    TermExitLinger,
    TermSessionGc,
    TermConfig,
    UploadMax,
    UploadTtl,
    VoiceStt,
    VoiceTts,
    VoiceInstructions,
}

impl Alias {
    const ALL: [Self; 23] = [
        Self::LogLevel,
        Self::NodeName,
        Self::NodeId,
        Self::PostgresUrl,
        Self::EmbedUrl,
        Self::EmbedModel,
        Self::Host,
        Self::UsersFile,
        Self::SharedDir,
        Self::InstallRoot,
        Self::TermMux,
        Self::TermMax,
        Self::TermScrollback,
        Self::TermDetachedTtl,
        Self::TermIdleTtl,
        Self::TermExitLinger,
        Self::TermSessionGc,
        Self::TermConfig,
        Self::UploadMax,
        Self::UploadTtl,
        Self::VoiceStt,
        Self::VoiceTts,
        Self::VoiceInstructions,
    ];

    const fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::LogLevel => (LOG_LEVEL_VARIABLE, "RIVETOS_LOG_LEVEL"),
            Self::NodeName => (NODE_NAME_VARIABLE, "RIVETOS_DEN_NODE_NAME"),
            Self::NodeId => (NODE_ID_VARIABLE, "RIVETOS_DEN_NODE_ID"),
            Self::PostgresUrl => (POSTGRES_URL_VARIABLE, "RIVETOS_PG_URL"),
            Self::EmbedUrl => ("RIVETHUB_EMBED_URL", "RIVETOS_EMBED_URL"),
            Self::EmbedModel => ("RIVETHUB_EMBED_MODEL", "RIVETOS_EMBED_MODEL"),
            Self::Host => ("RIVETHUB_HOST", "RIVETOS_HOST"),
            Self::UsersFile => ("RIVETHUB_USERS_FILE", "RIVETOS_USERS_FILE"),
            Self::SharedDir => ("RIVETHUB_SHARED_DIR", "RIVETOS_SHARED_DIR"),
            Self::InstallRoot => ("RIVETHUB_INSTALL_ROOT", "RIVETOS_INSTALL_ROOT"),
            Self::TermMux => ("RIVETHUB_TERM_MUX", "RIVETOS_TERM_MUX"),
            Self::TermMax => ("RIVETHUB_TERM_MAX", "RIVETOS_DEN_TERM_MAX"),
            Self::TermScrollback => ("RIVETHUB_TERM_SCROLLBACK", "RIVETOS_DEN_TERM_SCROLLBACK"),
            Self::TermDetachedTtl => (
                "RIVETHUB_TERM_DETACHED_TTL_MS",
                "RIVETOS_DEN_TERM_DETACHED_TTL_MS",
            ),
            Self::TermIdleTtl => ("RIVETHUB_TERM_IDLE_TTL_MS", "RIVETOS_DEN_TERM_IDLE_TTL_MS"),
            Self::TermExitLinger => (
                "RIVETHUB_TERM_EXIT_LINGER_MS",
                "RIVETOS_DEN_TERM_EXIT_LINGER_MS",
            ),
            Self::TermSessionGc => (
                "RIVETHUB_TERM_SESSION_GC_MS",
                "RIVETOS_DEN_TERM_SESSION_GC_MS",
            ),
            Self::TermConfig => ("RIVETHUB_TERM_CONFIG", "RIVETOS_DEN_TERM_CONFIG"),
            Self::UploadMax => (UPLOAD_MAX_BYTES_VARIABLE, "RIVETOS_DEN_UPLOAD_MAX_BYTES"),
            Self::UploadTtl => (UPLOAD_TTL_MS_VARIABLE, "RIVETOS_DEN_UPLOAD_TTL"),
            Self::VoiceStt => ("RIVETHUB_VOICE_STT_URL", "RIVETOS_DEN_VOICE_STT_URL"),
            Self::VoiceTts => ("RIVETHUB_VOICE_TTS_URL", "RIVETOS_DEN_VOICE_TTS_URL"),
            Self::VoiceInstructions => (
                "RIVETHUB_VOICE_TTS_INSTRUCTIONS",
                "RIVETOS_DEN_VOICE_TTS_INSTRUCTIONS",
            ),
        }
    }
}

#[must_use]
pub fn deprecated_alias_names() -> Vec<(&'static str, &'static str)> {
    Alias::ALL.into_iter().map(Alias::names).collect()
}

impl Environment {
    pub fn from_vars<V, I>(vars: I) -> Self
    where
        V: AsRef<OsStr>,
        I: IntoIterator<Item = (V, V)>,
    {
        Self {
            vars: vars
                .into_iter()
                .map(|(key, value)| (key.as_ref().to_owned(), value.as_ref().to_owned()))
                .collect(),
        }
    }

    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars_os().collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    #[must_use]
    pub fn with_override(mut self, name: &str, value: impl AsRef<OsStr>) -> Self {
        self.vars
            .insert(OsString::from(name), value.as_ref().to_owned());
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.vars.iter()
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(OsStr::to_str)
    }

    pub fn install_root(&self) -> Option<&str> {
        self.setting(Alias::InstallRoot)
    }

    pub fn users_file(&self) -> Option<&OsStr> {
        let (current, legacy) = Alias::UsersFile.names();
        self.get(current).or_else(|| self.get(legacy))
    }

    #[must_use]
    pub fn term_roster_path(&self) -> Option<PathBuf> {
        self.setting(Alias::TermConfig).map(PathBuf::from)
    }

    pub fn home(&self) -> Option<&str> {
        self.text("HOME")
    }

    #[must_use]
    pub fn log_level(&self) -> LogLevelSetting {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B10: unknown log levels use info and report the ignored setting"
            )
        )]
        match self.setting(Alias::LogLevel) {
            None => LogLevelSetting {
                level: LogLevel::Info,
                ignored: None,
            },
            Some(value) => match value.parse::<LogLevel>() {
                Ok(level) => LogLevelSetting {
                    level,
                    ignored: None,
                },
                Err(_) => LogLevelSetting {
                    level: LogLevel::Info,
                    ignored: Some(value.to_owned()),
                },
            },
        }
    }

    pub fn node_name(&self, hostname: &str) -> Result<String, crate::ConfigError> {
        self.setting(Alias::NodeName)
            .or_else(|| self.setting(Alias::NodeId))
            .or_else(|| self.trimmed("HOSTNAME"))
            .or_else(|| Some(hostname.trim()).filter(|name| !name.is_empty()))
            .map(str::to_owned)
            .ok_or(crate::ConfigError::NodeName)
    }

    pub fn postgres_url(&self) -> Option<String> {
        self.setting(Alias::PostgresUrl).map(str::to_owned)
    }

    pub fn embed_url(&self) -> Option<String> {
        self.setting(Alias::EmbedUrl).map(str::to_owned)
    }

    pub fn embed_model(&self) -> Option<String> {
        self.setting(Alias::EmbedModel).map(str::to_owned)
    }

    pub(crate) fn configured_shared_dir(&self) -> Option<&str> {
        self.setting(Alias::SharedDir)
    }

    fn setting(&self, alias: Alias) -> Option<&str> {
        self.named_setting(alias).map(|(_, value)| value)
    }

    fn named_setting(&self, alias: Alias) -> Option<(&'static str, &str)> {
        let (current, legacy) = alias.names();
        if let Some(value) = self.trimmed(current) {
            return Some((current, value));
        }
        self.trimmed(legacy).map(|value| (legacy, value))
    }

    #[must_use]
    pub fn voice_endpoints(&self) -> VoiceEndpoints {
        VoiceEndpoints {
            stt_url: self.setting(Alias::VoiceStt).map(str::to_owned),
            tts_url: self.setting(Alias::VoiceTts).map(str::to_owned),
            instructions: self.setting(Alias::VoiceInstructions).map(str::to_owned),
        }
    }

    fn trimmed(&self, name: &str) -> Option<&str> {
        self.text(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn term_mux(&self) -> Option<&str> {
        match self.setting(Alias::TermMux) {
            Some("tmux") | None => None,
            Some(value) => Some(value),
        }
    }

    pub fn term_mux_choice(&self) -> TermMuxChoice {
        match self.setting(Alias::TermMux) {
            None | Some("tmux") => TermMuxChoice {
                setting: TermMuxSetting::Auto,
                ignored: None,
            },
            Some("herdr") => TermMuxChoice {
                setting: TermMuxSetting::Herdr,
                ignored: None,
            },
            Some("none") => TermMuxChoice {
                setting: TermMuxSetting::Direct,
                ignored: None,
            },
            Some(value) => TermMuxChoice {
                setting: TermMuxSetting::Direct,
                ignored: Some(value.to_owned()),
            },
        }
    }

    pub fn term_settings(&self) -> TermSettings {
        let mut settings = TermSettings::defaults();
        let mut ignored = Vec::new();
        self.apply_term(Alias::TermMax, &mut settings.max_ptys, &mut ignored);
        self.apply_term(
            Alias::TermScrollback,
            &mut settings.scrollback_bytes,
            &mut ignored,
        );
        self.apply_term(
            Alias::TermDetachedTtl,
            &mut settings.detached_ttl_ms,
            &mut ignored,
        );
        self.apply_term(Alias::TermIdleTtl, &mut settings.idle_ttl_ms, &mut ignored);
        self.apply_term(
            Alias::TermExitLinger,
            &mut settings.exit_linger_ms,
            &mut ignored,
        );
        self.apply_term(
            Alias::TermSessionGc,
            &mut settings.session_gc_ms,
            &mut ignored,
        );
        settings.ignored = ignored;
        settings
    }

    #[must_use]
    pub fn upload_settings(&self) -> UploadSettings {
        let mut settings = UploadSettings::defaults();
        let mut ignored = Vec::new();
        self.apply_positive(Alias::UploadMax, &mut settings.max_bytes, &mut ignored);
        let mut ttl_ms = DEFAULT_UPLOAD_TTL_MS;
        self.apply_positive(Alias::UploadTtl, &mut ttl_ms, &mut ignored);
        settings.ttl = std::time::Duration::from_millis(ttl_ms);
        settings.ignored = ignored;
        settings
    }

    fn apply_positive(&self, alias: Alias, slot: &mut u64, ignored: &mut Vec<String>) {
        let (current, _) = alias.names();
        let Some(text) = self.setting(alias) else {
            return;
        };
        match text.parse::<u64>() {
            Ok(value) if value > 0 => *slot = value,
            Ok(_) => ignored.push(format!("{current}={text}")),
            Err(_error) => ignored.push(format!("{current}={text}")),
        }
    }

    fn apply_term<T>(&self, alias: Alias, slot: &mut T, ignored: &mut Vec<String>)
    where
        T: std::str::FromStr + Copy,
    {
        match self.term_value::<T>(alias) {
            Ok(Some(value)) => *slot = value,
            Ok(None) => {}
            Err(entry) => ignored.push(entry),
        }
    }

    fn term_value<T: std::str::FromStr>(&self, alias: Alias) -> Result<Option<T>, String> {
        let (current, _) = alias.names();
        let Some(text) = self.setting(alias) else {
            return Ok(None);
        };
        text.parse::<T>()
            .map(Some)
            .map_err(|_| format!("{current}={text}"))
    }

    pub fn legacy_notice(&self, new_name: &str, legacy_name: &str) -> Option<String> {
        if self.trimmed(new_name).is_some() || self.text(legacy_name).is_none() {
            return None;
        }
        Some(format!(
            "{legacy_name} is deprecated — rename it to {new_name}; the legacy name is read only while {new_name} is unset."
        ))
    }

    pub fn legacy_notices(&self) -> Vec<String> {
        Alias::ALL
            .into_iter()
            .filter_map(|alias| {
                let (current, legacy) = alias.names();
                self.legacy_notice(current, legacy)
            })
            .collect()
    }

    pub fn deprecation_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for tool in ["SHELL", "FILE", "SEARCH"] {
            for prefix in ["RIVETHUB", "RIVETOS"] {
                let name = format!("{prefix}_MCP_ENABLE_{tool}");
                if self.text(&name).is_some() {
                    warnings.push(format!(
                        "{name} is deprecated — built-in {} tools were removed (every harness ships its own) and the variable is ignored.",
                        tool.to_lowercase(),
                    ));
                }
            }
        }
        for suffix in [
            "INJECT_READY_MS",
            "INJECT_READY_MAX_MS",
            "INJECT_CONFIRM_MS",
            "INJECT_SUBMIT_DELAY_MS",
            "TMUX_USER_CONF",
        ] {
            for prefix in ["RIVETHUB", "RIVETOS"] {
                let name = format!("{prefix}_{suffix}");
                if self.text(&name).is_some() {
                    warnings.push(format!(
                        "{name} is deprecated — terminal injection settings were removed with tmux support and the variable is ignored."
                    ));
                }
            }
        }
        for name in ["RIVETHUB_TERM_MUX", "RIVETOS_TERM_MUX"] {
            if self.text(name) == Some("tmux") {
                warnings.push(format!(
                    "{name}=tmux is ignored — herdr is the only terminal multiplexer and the value is treated as unset."
                ));
            }
        }
        warnings
    }
}
