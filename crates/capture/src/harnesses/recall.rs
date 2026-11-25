use std::collections::BTreeMap;
use std::path::Path;

use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::SetupError;
use serde::Serialize;

use super::entries::JsonServers;
use super::entries::McpServer;
use super::entries::TomlServers;
use super::entries::YamlServers;
use super::json_edit;
use super::locate::Locator;
use super::plan::FileEdit;
use super::plan::read_optional;
use super::registry::EntryShape;
use super::registry::JsonKey;
use super::registry::Location;
use super::registry::Recall;
use super::toml_text;
use super::yaml_text;

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
        Recall::Json { .. } | Recall::Toml { .. } | Recall::Yaml { .. } | Recall::Dsh { .. }
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
        Recall::Yaml { .. } => serde_yaml_ng::from_str::<YamlServers>(text)
            .map(|doc| doc.mcp_servers.unwrap_or_default())
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

const YAML_SERVERS_KEY: &str = "mcp_servers";

#[derive(Serialize)]
struct YamlEntry<'a> {
    command: &'static str,
    args: [&'a str; 2],
}

fn yaml_entry(env_file: &Path, rh: &Path) -> Result<String, String> {
    let quote = |path: &Path| {
        shlex::try_quote(&path.to_string_lossy())
            .map(std::borrow::Cow::into_owned)
            .map_err(|error| error.to_string())
    };
    let script = format!(
        "set -a; . {} >/dev/null 2>&1; set +a; exec {} mcp",
        quote(env_file)?,
        quote(rh)?
    );
    serde_yaml_ng::to_string(&YamlEntry {
        command: "bash",
        args: ["-c", &script],
    })
    .map_err(|error| error.to_string())
}

fn yaml_body(locator: &Locator, target: &Path, rh: &Path) -> Result<String, SetupError> {
    let env_file = locator
        .env_file()
        .map_err(|error| config(target, format!("cannot find rh's env file: {error}")))?;
    yaml_entry(env_file, rh).map_err(|error| config(target, error))
}

fn yaml_snippet(locator: &Locator, target: &Path, rh: &Path) -> Result<String, SetupError> {
    let body = yaml_body(locator, target, rh)?;
    yaml_text::insert_child("", YAML_SERVERS_KEY, ENTRY_NAME, &body)
        .map_err(|error| config(target, error.to_string()))
}

fn yaml_document(path: &Path, text: &str) -> Result<serde_yaml_ng::Mapping, SetupError> {
    if text.trim().is_empty() {
        return Ok(serde_yaml_ng::Mapping::new());
    }
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|error| config(path, error.to_string()))?;
    if value.is_null() {
        return Ok(serde_yaml_ng::Mapping::new());
    }
    value
        .as_mapping()
        .cloned()
        .ok_or_else(|| config(path, "the document is not a map"))
}

fn yaml_servers_mut(document: &mut serde_yaml_ng::Mapping) -> Option<&mut serde_yaml_ng::Mapping> {
    let slot = document
        .entry(YAML_SERVERS_KEY.into())
        .or_insert(serde_yaml_ng::Value::Null);
    if slot.is_null() {
        *slot = serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new());
    }
    slot.as_mapping_mut()
}

fn refuse_merge_keys(
    path: &Path,
    text: &str,
    by_hand: impl FnOnce() -> Result<String, SetupError>,
) -> Result<(), SetupError> {
    let document = yaml_document(path, text)?;
    let merges = |map: &serde_yaml_ng::Mapping| map.contains_key("<<");
    let servers = document
        .get(YAML_SERVERS_KEY)
        .and_then(serde_yaml_ng::Value::as_mapping);
    if merges(&document) || servers.is_some_and(merges) {
        return Err(config(
            path,
            format!(
                "it uses YAML merge keys (<<), so rh will not edit it; {}",
                by_hand()?
            ),
        ));
    }
    Ok(())
}

fn verify_yaml(
    path: &Path,
    before: &str,
    after: &str,
    runs_rh: bool,
    change: impl FnOnce(&mut serde_yaml_ng::Mapping),
) -> Result<(), SetupError> {
    let mut expected = yaml_document(path, before)?;
    if let Some(servers) = yaml_servers_mut(&mut expected) {
        change(servers);
        if servers.is_empty() {
            expected.remove(YAML_SERVERS_KEY);
        }
    }
    let actual = yaml_document(path, after)?;
    let runs = serde_yaml_ng::from_str::<YamlServers>(after)
        .map_err(|error| config(path, error.to_string()))?
        .mcp_servers
        .unwrap_or_default()
        .values()
        .any(McpServer::runs_rh_mcp);
    if actual != expected || runs != runs_rh {
        return Err(config(
            path,
            "the edited YAML does not hold exactly the intended change, so nothing was written",
        ));
    }
    Ok(())
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
        Recall::Dsh { .. } => {
            let (edits, skipped) = super::dsh::plan(locator, rh, &[ConnectPart::Recall], true)?;
            return match skipped {
                Some(error) => Err(error),
                None => Ok(edits),
            };
        }
        Recall::Json { file, .. } | Recall::Toml { file } | Recall::Yaml { file } => locator
            .target(*file)
            .ok_or_else(|| config(Path::new(""), "no home directory to write to"))?,
    };
    let before = read_optional(&target)?;
    if let (Recall::Yaml { .. }, Some(text)) = (recall, &before) {
        refuse_merge_keys(&target, text, || {
            yaml_snippet(locator, &target, rh)
                .map(|snippet| format!("add this to it by hand:\n{snippet}"))
        })?;
    }
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
        Recall::Yaml { .. } => {
            let body = yaml_body(locator, &target, rh)?;
            let text = yaml_text::insert_child(&current, YAML_SERVERS_KEY, ENTRY_NAME, &body)
                .map_err(|error| config(&target, error.to_string()))?;
            let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body)
                .map_err(|error| config(&target, error.to_string()))?;
            verify_yaml(&target, &current, &text, true, |map| {
                map.insert(ENTRY_NAME.into(), value);
            })?;
            text
        }
        Recall::Dsh { .. } | Recall::Unwritable(_) => current,
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
        Recall::Dsh { .. } => {
            return super::dsh::plan(locator, Path::new(""), &[ConnectPart::Recall], false)
                .map(|(edits, _)| edits);
        }
        Recall::Json { file, .. } | Recall::Toml { file } | Recall::Yaml { file } => {
            match locator.target(*file) {
                Some(target) => target,
                None => return Ok(Vec::new()),
            }
        }
    };
    let Some(before) = read_optional(&target)? else {
        return Ok(Vec::new());
    };
    if let Recall::Yaml { .. } = recall {
        refuse_merge_keys(&target, &before, || {
            Ok(format!(
                "remove the rh entries from {YAML_SERVERS_KEY} in it by hand"
            ))
        })?;
    }
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
        Recall::Yaml { .. } => {
            let text = yaml_text::remove_children(&before, YAML_SERVERS_KEY, |name| {
                ours.iter().any(|ours| ours == name)
            })
            .map_err(|error| config(&target, error.to_string()))?;
            verify_yaml(&target, &before, &text, false, |map| {
                for name in &ours {
                    map.remove(name.as_str());
                }
            })?;
            text
        }
        Recall::Dsh { .. } | Recall::Unwritable(_) => before.clone(),
    };
    Ok(vec![FileEdit {
        path: target,
        before: Some(before),
        after: Some(text),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    fn the_wrapper_loads_the_env_file_and_runs_rh_whatever_the_paths_hold()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let odd = dir.path().join("it's a \"dir\" $HOME & `x`");
        std::fs::create_dir_all(&odd)?;
        let env_file = odd.join(".env");
        std::fs::write(&env_file, "SECRET_FROM_ENV=loaded\n")?;
        let rh = odd.join("rh");
        std::fs::write(&rh, "#!/bin/sh\necho \"$SECRET_FROM_ENV $1\"\n")?;
        std::fs::set_permissions(&rh, std::fs::Permissions::from_mode(0o755))?;

        let body = yaml_entry(&env_file, &rh)?;
        assert!(!body.contains("loaded"));
        let entry: serde_yaml_ng::Value = serde_yaml_ng::from_str(&body)?;
        let args: Vec<String> =
            serde_yaml_ng::from_value(entry.get("args").cloned().unwrap_or_default())?;
        let out = Command::new("bash").args(&args).env_clear().output()?;
        assert_eq!(String::from_utf8_lossy(&out.stdout), "loaded mcp\n");

        let server: McpServer = serde_yaml_ng::from_str(&body)?;
        assert!(server.runs_rh_mcp());
        Ok(())
    }
}
