use std::collections::BTreeMap;
use std::path::Path;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::SetupError;
use serde::Serialize;

use super::entries::JsonServers;
use super::entries::McpServer;
use super::entries::TomlServers;
use super::json_edit;
use super::locate::Locator;
use super::plan::FileEdit;
use super::plan::read_optional;
use super::registry::EntryShape;
use super::registry::JsonKey;
use super::registry::Location;
use super::registry::Recall;
use super::toml_text;

pub(super) const ENTRY_NAME: &str = "rivethub";
const LEGACY_ENTRY_NAME: &str = "rivetos";

fn names_a_memory_server(name: &str) -> bool {
    name == ENTRY_NAME || name == LEGACY_ENTRY_NAME
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecallView {
    pub(super) state: RecallState,
    pub(super) detail: String,
}

fn config(path: &Path, message: impl Into<String>) -> SetupError {
    SetupError::Config {
        path: path.display().to_string(),
        message: message.into(),
    }
}

pub(super) fn writable(recall: &Recall) -> bool {
    matches!(
        recall,
        Recall::Json { .. } | Recall::Toml { .. } | Recall::Dsh { .. }
    )
}

fn servers(
    recall: &Recall,
    path: &Path,
    text: &str,
) -> Result<BTreeMap<String, McpServer>, SetupError> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    match recall {
        Recall::Json { key, .. } => {
            let doc: JsonServers =
                serde_json::from_str(text).map_err(|error| config(path, error.to_string()))?;
            Ok(match key {
                JsonKey::McpServers => doc.standard,
                JsonKey::Mcp => doc.mcp,
            })
        }
        Recall::Toml { .. } => toml::from_str::<TomlServers>(text)
            .map(|doc| doc.mcp_servers)
            .map_err(|error| config(path, error.to_string())),
        Recall::Yaml { .. } => serde_yaml_ng::from_str::<TomlServers>(text)
            .map(|doc| doc.mcp_servers)
            .map_err(|error| config(path, error.to_string())),
        Recall::Dsh { .. } | Recall::Unwritable(_) => Ok(BTreeMap::new()),
    }
}

fn file_of(recall: &Recall) -> Option<Location> {
    match recall {
        Recall::Json { file, .. }
        | Recall::Toml { file }
        | Recall::Yaml { file }
        | Recall::Dsh { file } => Some(*file),
        Recall::Unwritable(_) => None,
    }
}

pub(super) fn detect(locator: &Locator, recall: &Recall) -> RecallView {
    if let Recall::Dsh { .. } = recall {
        let (state, detail) = super::dsh::recall_status(locator);
        return RecallView { state, detail };
    }
    if let Recall::Unwritable(reason) = recall {
        return RecallView {
            state: RecallState::NotConnected,
            detail: (*reason).to_owned(),
        };
    }
    let Some(target) = file_of(recall).and_then(|file| locator.target(file)) else {
        return RecallView {
            state: RecallState::NotConnected,
            detail: "no home directory to look in".to_owned(),
        };
    };
    let text = match read_optional(&target) {
        Ok(Some(text)) => text,
        Ok(None) => {
            return RecallView {
                state: RecallState::NotConnected,
                detail: format!("no MCP config at {}", target.display()),
            };
        }
        Err(error) => {
            return RecallView {
                state: RecallState::NotConnected,
                detail: error.to_string(),
            };
        }
    };
    match servers(recall, &target, &text) {
        Err(error) => RecallView {
            state: RecallState::NotConnected,
            detail: format!("cannot read MCP config: {error}"),
        },
        Ok(entries) => {
            if let Some((name, _)) = entries.iter().find(|(_, server)| server.runs_rh_mcp()) {
                return RecallView {
                    state: RecallState::Connected,
                    detail: format!("{name} runs rh mcp in {}", target.display()),
                };
            }
            match entries.iter().find(|(name, _)| names_a_memory_server(name)) {
                Some((name, server)) => RecallView {
                    state: RecallState::Elsewhere,
                    detail: format!(
                        "{name} in {} runs {}, not rh mcp",
                        target.display(),
                        server.invocation()
                    ),
                },
                None => RecallView {
                    state: RecallState::NotConnected,
                    detail: format!("no rh mcp entry in {}", target.display()),
                },
            }
        }
    }
}

#[derive(Serialize)]
struct StdioEntry<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<&'static str>,
    command: &'a str,
    args: [&'static str; 1],
}

#[derive(Serialize)]
struct LocalEntry<'a> {
    r#type: &'static str,
    command: [&'a str; 2],
    enabled: bool,
}

#[derive(Serialize)]
struct TomlEntry<'a> {
    command: &'a str,
    args: [&'static str; 1],
}

fn entry_json(shape: EntryShape, rh: &str) -> Result<String, serde_json::Error> {
    match shape {
        EntryShape::ClaudeStdio => serde_json::to_string_pretty(&StdioEntry {
            r#type: Some("stdio"),
            command: rh,
            args: ["mcp"],
        }),
        EntryShape::Command => serde_json::to_string_pretty(&StdioEntry {
            r#type: None,
            command: rh,
            args: ["mcp"],
        }),
        EntryShape::Opencode => serde_json::to_string_pretty(&LocalEntry {
            r#type: "local",
            command: [rh, "mcp"],
            enabled: true,
        }),
    }
}

pub(super) fn plan_connect(
    locator: &Locator,
    id: &str,
    recall: &Recall,
    rh: &Path,
) -> Result<Vec<FileEdit>, SetupError> {
    let unsupported = |reason: &'static str| SetupError::Unsupported {
        id: id.to_owned(),
        part: ConnectPart::Recall,
        reason,
    };
    let target = match recall {
        Recall::Unwritable(reason) => return Err(unsupported(reason)),
        Recall::Yaml { .. } => {
            return Err(unsupported(
                "its MCP servers live in a YAML file rh does not edit yet",
            ));
        }
        Recall::Dsh { .. } => {
            let (edits, skipped) = super::dsh::plan(locator, rh, &[ConnectPart::Recall], true)?;
            return match skipped {
                Some(error) => Err(error),
                None => Ok(edits),
            };
        }
        Recall::Json { file, .. } | Recall::Toml { file } => locator
            .target(*file)
            .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?,
    };
    let before = read_optional(&target)?;
    let existing = match &before {
        Some(text) => servers(recall, &target, text)?,
        None => BTreeMap::new(),
    };
    if existing.values().any(McpServer::runs_rh_mcp) {
        return Ok(Vec::new());
    }
    if existing.contains_key(ENTRY_NAME) {
        return Err(config(
            &target,
            format!("an entry named {ENTRY_NAME} already runs something else"),
        ));
    }
    let rh_text = rh.to_string_lossy();
    let current = before.clone().unwrap_or_default();
    let mut text = match recall {
        Recall::Json { key, shape, .. } => {
            let entry =
                entry_json(*shape, &rh_text).map_err(|error| config(&target, error.to_string()))?;
            json_edit::insert_member(&current, &[key.name()], ENTRY_NAME, &entry)
                .map_err(|error| config(&target, error.to_string()))?
        }
        Recall::Toml { .. } => {
            let body = toml::to_string(&TomlEntry {
                command: &rh_text,
                args: ["mcp"],
            })
            .map_err(|error| config(&target, error.to_string()))?;
            toml_text::append_block(&current, &format!("[mcp_servers.{ENTRY_NAME}]\n{body}"))
        }
        Recall::Yaml { .. } | Recall::Dsh { .. } | Recall::Unwritable(_) => current,
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(vec![FileEdit {
        path: target,
        before,
        after: Some(text),
    }])
}

pub(super) fn plan_disconnect(
    locator: &Locator,
    id: &str,
    recall: &Recall,
) -> Result<Vec<FileEdit>, SetupError> {
    let target = match recall {
        Recall::Unwritable(reason) => {
            return Err(SetupError::Unsupported {
                id: id.to_owned(),
                part: ConnectPart::Recall,
                reason,
            });
        }
        Recall::Yaml { .. } => {
            return Err(SetupError::Unsupported {
                id: id.to_owned(),
                part: ConnectPart::Recall,
                reason: "its MCP servers live in a YAML file rh does not edit yet",
            });
        }
        Recall::Dsh { .. } => {
            return super::dsh::plan(locator, Path::new(""), &[ConnectPart::Recall], false)
                .map(|(edits, _)| edits);
        }
        Recall::Json { file, .. } | Recall::Toml { file } => match locator.target(*file) {
            Some(target) => target,
            None => return Ok(Vec::new()),
        },
    };
    let Some(before) = read_optional(&target)? else {
        return Ok(Vec::new());
    };
    let ours: Vec<String> = servers(recall, &target, &before)?
        .into_iter()
        .filter(|(_, server)| server.runs_rh_mcp())
        .map(|(name, _)| name)
        .collect();
    if ours.is_empty() {
        return Ok(Vec::new());
    }
    let text = match recall {
        Recall::Json { key, .. } => json_edit::remove_members(&before, &[key.name()], |name| {
            ours.iter().any(|ours| ours == name)
        })
        .and_then(|text| json_edit::prune_empty(&text, &[key.name()]))
        .map_err(|error| config(&target, error.to_string()))?,
        Recall::Toml { .. } => ours.iter().fold(before.clone(), |text, name| {
            toml_text::remove_tables(&text, "mcp_servers", name)
        }),
        Recall::Yaml { .. } | Recall::Dsh { .. } | Recall::Unwritable(_) => before.clone(),
    };
    Ok(vec![FileEdit {
        path: target,
        before: Some(before),
        after: Some(text),
    }])
}
