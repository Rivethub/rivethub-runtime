use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumIter)]
enum RetiredVariable {
    #[strum(serialize = "RIVETHUB_HOST")]
    RivethubHost,
    #[strum(serialize = "RIVETOS_HOST")]
    RivetosHost,
    #[strum(serialize = "RIVETHUB_TERM_MUX")]
    RivethubTermMux,
    #[strum(serialize = "RIVETOS_TERM_MUX")]
    RivetosTermMux,
    #[strum(serialize = "RIVETHUB_TERM_MAX")]
    RivethubTermMax,
    #[strum(serialize = "RIVETOS_DEN_TERM_MAX")]
    RivetosDenTermMax,
    #[strum(serialize = "RIVETHUB_TERM_SCROLLBACK")]
    RivethubTermScrollback,
    #[strum(serialize = "RIVETOS_DEN_TERM_SCROLLBACK")]
    RivetosDenTermScrollback,
    #[strum(serialize = "RIVETHUB_TERM_DETACHED_TTL_MS")]
    RivethubTermDetachedTtlMs,
    #[strum(serialize = "RIVETOS_DEN_TERM_DETACHED_TTL_MS")]
    RivetosDenTermDetachedTtlMs,
    #[strum(serialize = "RIVETHUB_TERM_IDLE_TTL_MS")]
    RivethubTermIdleTtlMs,
    #[strum(serialize = "RIVETOS_DEN_TERM_IDLE_TTL_MS")]
    RivetosDenTermIdleTtlMs,
    #[strum(serialize = "RIVETHUB_TERM_EXIT_LINGER_MS")]
    RivethubTermExitLingerMs,
    #[strum(serialize = "RIVETOS_DEN_TERM_EXIT_LINGER_MS")]
    RivetosDenTermExitLingerMs,
    #[strum(serialize = "RIVETHUB_TERM_SESSION_GC_MS")]
    RivethubTermSessionGcMs,
    #[strum(serialize = "RIVETOS_DEN_TERM_SESSION_GC_MS")]
    RivetosDenTermSessionGcMs,
    #[strum(serialize = "RIVETHUB_TERM_CONFIG")]
    RivethubTermConfig,
    #[strum(serialize = "RIVETOS_DEN_TERM_CONFIG")]
    RivetosDenTermConfig,
    #[strum(serialize = "RIVETHUB_UPLOAD_MAX_BYTES")]
    RivethubUploadMaxBytes,
    #[strum(serialize = "RIVETOS_DEN_UPLOAD_MAX_BYTES")]
    RivetosDenUploadMaxBytes,
    #[strum(serialize = "RIVETHUB_UPLOAD_TTL_MS")]
    RivethubUploadTtlMs,
    #[strum(serialize = "RIVETOS_DEN_UPLOAD_TTL")]
    RivetosDenUploadTtl,
    #[strum(serialize = "RIVETHUB_VOICE_STT_URL")]
    RivethubVoiceSttUrl,
    #[strum(serialize = "RIVETOS_DEN_VOICE_STT_URL")]
    RivetosDenVoiceSttUrl,
    #[strum(serialize = "RIVETHUB_VOICE_TTS_URL")]
    RivethubVoiceTtsUrl,
    #[strum(serialize = "RIVETOS_DEN_VOICE_TTS_URL")]
    RivetosDenVoiceTtsUrl,
    #[strum(serialize = "RIVETHUB_VOICE_TTS_INSTRUCTIONS")]
    RivethubVoiceTtsInstructions,
    #[strum(serialize = "RIVETOS_DEN_VOICE_TTS_INSTRUCTIONS")]
    RivetosDenVoiceTtsInstructions,
}

pub const CONFIG_PATH_VARIABLE: &str = "RIVETHUB_CONFIG";
pub const LOG_LEVEL_VARIABLE: &str = "RIVETHUB_LOG_LEVEL";
pub const NODE_NAME_VARIABLE: &str = "RIVETHUB_NODE_NAME";
pub const NODE_ID_VARIABLE: &str = "RIVETHUB_NODE_ID";
pub const MODEL_RUN_VARIABLE: &str = "RIVETHUB_MODEL_RUN";
pub const POSTGRES_URL_VARIABLE: &str = "RIVETHUB_PG_URL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alias {
    LogLevel,
    NodeName,
    NodeId,
    PostgresUrl,
    EmbedUrl,
    EmbedModel,
    UsersFile,
    SharedDir,
    InstallRoot,
}

impl Alias {
    const ALL: [Self; 9] = [
        Self::LogLevel,
        Self::NodeName,
        Self::NodeId,
        Self::PostgresUrl,
        Self::EmbedUrl,
        Self::EmbedModel,
        Self::UsersFile,
        Self::SharedDir,
        Self::InstallRoot,
    ];

    const fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::LogLevel => (LOG_LEVEL_VARIABLE, "RIVETOS_LOG_LEVEL"),
            Self::NodeName => (NODE_NAME_VARIABLE, "RIVETOS_DEN_NODE_NAME"),
            Self::NodeId => (NODE_ID_VARIABLE, "RIVETOS_DEN_NODE_ID"),
            Self::PostgresUrl => (POSTGRES_URL_VARIABLE, "RIVETOS_PG_URL"),
            Self::EmbedUrl => ("RIVETHUB_EMBED_URL", "RIVETOS_EMBED_URL"),
            Self::EmbedModel => ("RIVETHUB_EMBED_MODEL", "RIVETOS_EMBED_MODEL"),
            Self::UsersFile => ("RIVETHUB_USERS_FILE", "RIVETOS_USERS_FILE"),
            Self::SharedDir => ("RIVETHUB_SHARED_DIR", "RIVETOS_SHARED_DIR"),
            Self::InstallRoot => ("RIVETHUB_INSTALL_ROOT", "RIVETOS_INSTALL_ROOT"),
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

    fn trimmed(&self, name: &str) -> Option<&str> {
        self.text(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
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
        for retired in <RetiredVariable as strum::IntoEnumIterator>::iter() {
            let name = retired.to_string();
            if self.text(&name).is_some() {
                warnings.push(format!(
                    "{name} is no longer used — rh runs no mesh, terminals, uploads or voice, and the variable is ignored."
                ));
            }
        }
        warnings
    }
}
