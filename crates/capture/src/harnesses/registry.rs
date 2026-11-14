use kernel::harness_setup::CaptureMethod;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Home,
    XdgConfig,
    OpencodeConfig,
    ClaudeDir,
    ClaudeJsonDir,
    Grok,
    Hermes,
    Kimi,
    Codex,
    OpencodeData,
    Omp,
    Cursor,
    Dsh,
    Cowork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub base: Base,
    pub relative: &'static str,
}

const fn at(base: Base, relative: &'static str) -> Location {
    Location { base, relative }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStyle {
    Line,
    Argv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEvent {
    pub name: &'static str,
    pub matcher: Option<&'static str>,
}

const fn event(name: &'static str) -> HookEvent {
    HookEvent {
        name,
        matcher: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookFileSpec {
    pub file: Location,
    pub events: &'static [HookEvent],
    pub style: CommandStyle,
    pub timeout: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInstall {
    HookFile(HookFileSpec),
    OpencodePlugin { directory: Location },
    DshPlugin { directory: Location },
}

impl CaptureInstall {
    #[must_use]
    pub const fn writable(&self) -> bool {
        match self {
            Self::HookFile(_) | Self::OpencodePlugin { .. } | Self::DshPlugin { .. } => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    ClaudeStdio,
    Command,
    Opencode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recall {
    Json {
        file: Location,
        key: JsonKey,
        shape: EntryShape,
    },
    Toml {
        file: Location,
    },
    Yaml {
        file: Location,
    },
    Dsh {
        file: Location,
    },
    Unwritable(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonKey {
    McpServers,
    Mcp,
}

impl JsonKey {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::McpServers => "mcpServers",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HarnessSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub session_prefix: &'static str,
    pub binaries: &'static [&'static str],
    pub data: &'static [Location],
    pub capture: &'static [CaptureMethod],
    pub install: Option<CaptureInstall>,
    pub recall: Recall,
}

const CLAUDE_EVENTS: &[HookEvent] = &[
    event("UserPromptSubmit"),
    HookEvent {
        name: "PostToolUse",
        matcher: Some("*"),
    },
    event("Stop"),
    event("SubagentStop"),
    event("SessionEnd"),
];
const CODEX_EVENTS: &[HookEvent] = &[event("Stop"), event("SessionEnd")];
const GROK_EVENTS: &[HookEvent] = &[event("Stop"), event("PreCompact"), event("SessionEnd")];

pub const REGISTRY: [HarnessSpec; 12] = [
    HarnessSpec {
        id: "claude-code",
        name: "Claude Code",
        session_prefix: "claude-code",
        binaries: &["claude"],
        data: &[at(Base::ClaudeDir, "")],
        capture: &[CaptureMethod::Hook, CaptureMethod::StoreScan],
        install: Some(CaptureInstall::HookFile(HookFileSpec {
            file: at(Base::ClaudeDir, "settings.json"),
            events: CLAUDE_EVENTS,
            style: CommandStyle::Line,
            timeout: 30,
        })),
        recall: Recall::Json {
            file: at(Base::ClaudeJsonDir, ".claude.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::ClaudeStdio,
        },
    },
    HarnessSpec {
        id: "codex",
        name: "Codex",
        session_prefix: "codex",
        binaries: &["codex"],
        data: &[at(Base::Codex, "")],
        capture: &[CaptureMethod::Hook, CaptureMethod::StoreScan],
        install: Some(CaptureInstall::HookFile(HookFileSpec {
            file: at(Base::Codex, "hooks.json"),
            events: CODEX_EVENTS,
            style: CommandStyle::Line,
            timeout: 20,
        })),
        recall: Recall::Toml {
            file: at(Base::Codex, "config.toml"),
        },
    },
    HarnessSpec {
        id: "grok-build",
        name: "Grok Build",
        session_prefix: "grok-build",
        binaries: &["grok"],
        data: &[at(Base::Grok, "")],
        capture: &[CaptureMethod::Hook, CaptureMethod::StoreScan],
        install: Some(CaptureInstall::HookFile(HookFileSpec {
            file: at(Base::Grok, "hooks/rivethub-capture.json"),
            events: GROK_EVENTS,
            style: CommandStyle::Argv,
            timeout: 30,
        })),
        recall: Recall::Toml {
            file: at(Base::Grok, "config.toml"),
        },
    },
    HarnessSpec {
        id: "opencode",
        name: "OpenCode",
        session_prefix: "opencode",
        binaries: &["opencode"],
        data: &[at(Base::OpencodeData, ""), at(Base::OpencodeConfig, "")],
        capture: &[CaptureMethod::Plugin, CaptureMethod::StoreScan],
        install: Some(CaptureInstall::OpencodePlugin {
            directory: at(Base::OpencodeConfig, "plugins"),
        }),
        recall: Recall::Json {
            file: at(Base::OpencodeConfig, "opencode.json"),
            key: JsonKey::Mcp,
            shape: EntryShape::Opencode,
        },
    },
    HarnessSpec {
        id: "cursor",
        name: "Cursor",
        session_prefix: "cursor",
        binaries: &["cursor-agent", "cursor"],
        data: &[at(Base::Cursor, "")],
        capture: &[CaptureMethod::StoreScan],
        install: None,
        recall: Recall::Json {
            file: at(Base::Cursor, "mcp.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
    HarnessSpec {
        id: "dsh",
        name: "DeepSeek Harness",
        session_prefix: "dsh",
        binaries: &["dsh"],
        data: &[at(Base::Dsh, "")],
        capture: &[CaptureMethod::Hook, CaptureMethod::StoreScan],
        install: Some(CaptureInstall::DshPlugin {
            directory: at(Base::Dsh, "plugins"),
        }),
        recall: Recall::Dsh {
            file: at(Base::Dsh, "cordis.patch.yml"),
        },
    },
    HarnessSpec {
        id: "cowork",
        name: "Claude Cowork",
        session_prefix: "cowork",
        binaries: &[],
        data: &[at(Base::Cowork, "")],
        capture: &[CaptureMethod::StoreScan],
        install: None,
        recall: Recall::Json {
            file: at(Base::Cowork, "claude_desktop_config.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
    HarnessSpec {
        id: "kimi",
        name: "Kimi Code",
        session_prefix: "kimi-code",
        binaries: &["kimi"],
        data: &[at(Base::Kimi, "")],
        capture: &[CaptureMethod::StoreScan],
        install: None,
        recall: Recall::Json {
            file: at(Base::Kimi, "mcp.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
    HarnessSpec {
        id: "omp",
        name: "Oh My Pi",
        session_prefix: "omp",
        binaries: &["omp"],
        data: &[at(Base::Omp, "")],
        capture: &[CaptureMethod::StoreScan],
        install: None,
        recall: Recall::Json {
            file: at(Base::Omp, "mcp.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
    HarnessSpec {
        id: "hermes",
        name: "Hermes",
        session_prefix: "hermes",
        binaries: &["hermes"],
        data: &[at(Base::Hermes, "")],
        capture: &[CaptureMethod::StoreScan],
        install: None,
        recall: Recall::Yaml {
            file: at(Base::Hermes, "config.yaml"),
        },
    },
    HarnessSpec {
        id: "antigravity",
        name: "Antigravity",
        session_prefix: "antigravity",
        binaries: &["antigravity"],
        data: &[at(Base::Home, ".gemini/antigravity")],
        capture: &[CaptureMethod::ClientPush],
        install: None,
        recall: Recall::Json {
            file: at(Base::Home, ".gemini/antigravity/mcp_config.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
    HarnessSpec {
        id: "kiro",
        name: "Kiro",
        session_prefix: "kiro",
        binaries: &["kiro", "kiro-cli"],
        data: &[at(Base::Home, ".kiro")],
        capture: &[CaptureMethod::ClientPush],
        install: None,
        recall: Recall::Json {
            file: at(Base::Home, ".kiro/settings/mcp.json"),
            key: JsonKey::McpServers,
            shape: EntryShape::Command,
        },
    },
];

#[must_use]
pub fn spec(id: &str) -> Option<&'static HarnessSpec> {
    REGISTRY.iter().find(|spec| spec.id == id)
}
