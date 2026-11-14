use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

use kernel::harness_setup::SetupError;

use super::entries::capture_hook_harness;
use super::install::InstallView;
use super::install::view;
use super::locate::Locator;
use super::plan::read_optional;
use super::registry::HookFileSpec;
use super::version::Version;

const TRUST_SINCE: Version = Version::new(0, 129, 0);
const HASH_CHECKED_FROM: Version = Version::new(0, 169, 0);
const HASH_CHECKED_BEFORE: Version = Version::new(0, 170, 0);

pub(super) const TRUST_STEP: &str = "the next time you open `codex` it shows \"Hooks need review\"; choose \"Trust all and continue\" (or review rh's two hooks and trust them). Until then Codex skips them, and `codex exec` runs are not captured";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Trust {
    Trusted,
    Unknown,
    Waiting,
    Changed,
    TurnedOff,
}

#[derive(Debug, Default, Deserialize)]
struct CodexConfig {
    #[serde(default)]
    hooks: CodexHooks,
}

#[derive(Debug, Default, Deserialize)]
struct CodexHooks {
    #[serde(default)]
    state: BTreeMap<String, HookState>,
}

#[derive(Debug, Default, Deserialize)]
struct HookState {
    enabled: Option<bool>,
    trusted_hash: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexHookFile {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<CodexGroup>>,
}

#[derive(Debug, Deserialize)]
struct CodexGroup {
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    hooks: Vec<CodexHandler>,
}

#[derive(Debug, Deserialize)]
struct CodexHandler {
    #[serde(rename = "type")]
    kind: String,
    command: Option<String>,
    timeout: Option<u64>,
    #[serde(default, rename = "async")]
    run_async: bool,
    #[serde(rename = "statusMessage")]
    status_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Stop,
    SessionEnd,
}

impl Event {
    fn of(name: &str) -> Option<Self> {
        match name {
            "Stop" => Some(Self::Stop),
            "SessionEnd" => Some(Self::SessionEnd),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::SessionEnd => "session_end",
        }
    }

    fn timeout(self, written: Option<u64>) -> u64 {
        match self {
            Self::Stop => written.unwrap_or(600).max(1),
            Self::SessionEnd => written.unwrap_or(1).clamp(1, 3),
        }
    }
}

#[derive(Serialize)]
struct Identity<'a> {
    event_name: &'static str,
    hooks: [Handler<'a>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    matcher: Option<String>,
}

#[derive(Serialize)]
struct Handler<'a> {
    #[serde(rename = "async")]
    run_async: bool,
    command: &'a str,
    #[serde(rename = "statusMessage", skip_serializing_if = "Option::is_none")]
    status_message: Option<&'a str>,
    timeout: u64,
    #[serde(rename = "type")]
    kind: &'static str,
}

fn hash(
    event: Event,
    group: &CodexGroup,
    hook: &CodexHandler,
) -> Result<Option<String>, serde_json::Error> {
    if hook.kind != "command" {
        return Ok(None);
    }
    let Some(command) = hook.command.as_deref() else {
        return Ok(None);
    };
    let matcher = match event {
        Event::Stop => None,
        Event::SessionEnd => group.matcher.clone(),
    };
    let identity = Identity {
        event_name: event.label(),
        hooks: [Handler {
            run_async: hook.run_async,
            command,
            status_message: hook.status_message.as_deref(),
            timeout: event.timeout(hook.timeout),
            kind: "command",
        }],
        matcher,
    };
    let json = serde_json::to_vec(&identity)?;
    Ok(Some(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(json))
    )))
}

fn same_file(key_path: &str, hooks: &Path) -> bool {
    let key_path = Path::new(key_path);
    key_path == hooks
        || matches!(
            (key_path.canonicalize(), hooks.canonicalize()),
            (Ok(left), Ok(right)) if left == right
        )
}

fn state_for<'a>(
    config: &'a CodexConfig,
    hooks: &Path,
    event: Event,
    group: usize,
    handler: usize,
) -> Option<&'a HookState> {
    let tail = format!(":{}:{group}:{handler}", event.label());
    config.hooks.state.iter().find_map(|(key, state)| {
        key.strip_suffix(&tail)
            .filter(|path| same_file(path, hooks))
            .map(|_| state)
    })
}

fn judge(state: Option<&HookState>, current: Option<&str>) -> Trust {
    match (state, current) {
        (Some(state), _) if state.enabled == Some(false) => Trust::TurnedOff,
        (
            None
            | Some(HookState {
                trusted_hash: None, ..
            }),
            _,
        ) => Trust::Waiting,
        (
            Some(HookState {
                trusted_hash: Some(trusted),
                ..
            }),
            Some(current),
        ) if trusted == current => Trust::Trusted,
        (Some(_), Some(_)) => Trust::Changed,
        (Some(_), None) => Trust::Unknown,
    }
}

#[derive(Debug, Error)]
enum TrustUnread {
    #[error("{0}")]
    Read(#[from] SetupError),
    #[error("hooks.json: {0}")]
    Hooks(#[from] serde_json::Error),
    #[error("config.toml: {0}")]
    Config(#[from] toml::de::Error),
}

fn assess(
    hooks: &Path,
    hooks_text: &str,
    config_text: Option<&str>,
    version: Option<Version>,
) -> Result<Trust, TrustUnread> {
    let file = serde_json::from_str::<CodexHookFile>(hooks_text)?;
    let config = match config_text {
        None => CodexConfig::default(),
        Some(text) => toml::from_str::<CodexConfig>(text)?,
    };
    let mut verdicts = Vec::new();
    for (name, groups) in &file.hooks {
        let Some(event) = Event::of(name) else {
            continue;
        };
        for (group_index, group) in groups.iter().enumerate() {
            for (handler_index, hook) in group.hooks.iter().enumerate() {
                let runs_rh = hook
                    .command
                    .as_deref()
                    .and_then(capture_hook_harness)
                    .is_some_and(|harness| harness == "codex");
                if !runs_rh {
                    continue;
                }
                let state = state_for(&config, hooks, event, group_index, handler_index);
                verdicts.push(judge(state, hash(event, group, hook)?.as_deref()));
            }
        }
    }
    let Some(worst) = verdicts.into_iter().max() else {
        return Ok(Trust::Unknown);
    };
    Ok(match (worst, version) {
        (Trust::Trusted, _) => Trust::Trusted,
        (_, None) => Trust::Unknown,
        (_, Some(version)) if version < TRUST_SINCE => Trust::Unknown,
        (Trust::Changed, Some(version))
            if !(HASH_CHECKED_FROM..HASH_CHECKED_BEFORE).contains(&version) =>
        {
            Trust::Unknown
        }
        (verdict, Some(_)) => verdict,
    })
}

fn assess_files(target: &Path, version: Option<Version>) -> Result<Trust, TrustUnread> {
    let Some(hooks_text) = read_optional(target)? else {
        return Ok(Trust::Unknown);
    };
    let config_text = read_optional(&target.with_file_name("config.toml"))?;
    assess(target, &hooks_text, config_text.as_deref(), version)
}

pub(super) fn read(locator: &Locator, hooks: &HookFileSpec, version: Option<Version>) -> Trust {
    let Some(target) = locator.target(hooks.file) else {
        return Trust::Unknown;
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: rh says nothing about Codex hook trust when it cannot read or parse Codex's hooks.json or config.toml (CONTRACT note 205)"
        )
    )]
    let trust = assess_files(&target, version).unwrap_or(Trust::Unknown);
    trust
}

pub(super) fn overlay(found: InstallView, trust: Trust) -> InstallView {
    if !found.installed {
        return found;
    }
    let detail = found.detail;
    let waiting = |detail: String| InstallView {
        installed: false,
        connectable: false,
        detail,
    };
    match trust {
        Trust::Unknown => view(true, detail),
        Trust::Trusted => view(true, format!("{detail}, trusted in Codex")),
        Trust::Waiting => waiting(format!(
            "{detail}, waiting for you to trust it in Codex: {TRUST_STEP}"
        )),
        Trust::Changed => waiting(format!(
            "{detail}, changed since you trusted it in Codex: {TRUST_STEP}"
        )),
        Trust::TurnedOff => waiting(format!(
            "{detail}, turned off in Codex; turn it back on with /hooks in `codex`"
        )),
    }
}

pub(super) fn connect_note(trust: Trust) -> Option<String> {
    match trust {
        Trust::Trusted => None,
        Trust::Waiting | Trust::Changed => Some(format!(
            "Codex runs these hooks only after you trust them: {TRUST_STEP}."
        )),
        Trust::TurnedOff => Some(
            "Codex has rh's hooks turned off; turn them back on with /hooks in `codex`, or nothing is captured.".to_owned(),
        ),
        Trust::Unknown => Some(
            "If Codex shows \"Hooks need review\" the next time you open it, choose \"Trust all and continue\": Codex skips hooks it has not trusted, `codex exec` runs included.".to_owned(),
        ),
    }
}
