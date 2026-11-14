use std::path::Path;
use std::path::PathBuf;

use kernel::harness_setup::RecallState;
use serde_yaml_ng::Value;

use super::super::locate::Locator;
use super::super::recall::ENTRY_NAME;
use super::Coverage;
use super::Paths;
use super::patch::names_our_recall;
use super::patch::read_patch;
use super::patch::row_runs_rh;
use super::patch::row_server;
use super::patch::rows_of;
use super::paths;
use super::profile::legacy_capture;
use super::profile::legacy_recall;
use super::profile::profile_names;
use super::profile::seen_in;

pub(in super::super) fn capture_status(locator: &Locator) -> Coverage {
    let paths = match paths(locator) {
        Ok(paths) => paths,
        Err(error) => {
            return Coverage {
                installed: false,
                detail: format!("no home directory to look in: {error}"),
            };
        }
    };
    let profiles = profile_names(&paths.root);
    match home_capture(&paths) {
        Home::Covers => Coverage {
            installed: true,
            detail: cover_detail(&paths.patch, &profiles),
        },
        Home::Missing(path) => Coverage {
            installed: false,
            detail: format!("home patch names a missing plug-in at {}", path.display()),
        },
        Home::Unread(error) => Coverage {
            installed: false,
            detail: format!("cannot read plug-ins: {error}"),
        },
        Home::Absent => Coverage {
            installed: false,
            detail: capture_gap(&paths, &profiles),
        },
    }
}

pub(in super::super) fn recall_status(locator: &Locator) -> (RecallState, String) {
    let paths = match paths(locator) {
        Ok(paths) => paths,
        Err(error) => {
            return (
                RecallState::NotConnected,
                format!("no home directory to look in: {error}"),
            );
        }
    };
    let profiles = profile_names(&paths.root);
    match home_recall(&paths) {
        RecallHome::Connected(name) => (
            RecallState::Connected,
            format!(
                "{name} runs rh mcp in {} (every profile, including ones created later)",
                paths.patch.display()
            ),
        ),
        RecallHome::Elsewhere { name, invocation } => (
            RecallState::Elsewhere,
            format!(
                "{name} in {} runs {invocation}, not rh mcp",
                paths.patch.display()
            ),
        ),
        RecallHome::Unread(error) => (
            RecallState::NotConnected,
            format!("cannot read MCP config: {error}"),
        ),
        RecallHome::Absent => (RecallState::NotConnected, recall_gap(&paths, &profiles)),
    }
}

enum Home {
    Covers,
    Missing(PathBuf),
    Unread(String),
    Absent,
}

enum RecallHome {
    Connected(String),
    Elsewhere { name: String, invocation: String },
    Unread(String),
    Absent,
}

fn home_capture(paths: &Paths) -> Home {
    match read_patch(&paths.patch) {
        Err(error) => Home::Unread(error),
        Ok(text) => match seen_in(&paths.root, &text) {
            Err(error) => Home::Unread(error),
            Ok(seen) if seen.capture.marker => Home::Covers,
            Ok(seen) => seen.capture.missing.map_or(Home::Absent, Home::Missing),
        },
    }
}

fn home_recall(paths: &Paths) -> RecallHome {
    match read_patch(&paths.patch) {
        Err(error) => RecallHome::Unread(error),
        Ok(text) => match seen_in(&paths.root, &text) {
            Err(error) => RecallHome::Unread(error),
            Ok(seen) if seen.recall.runs_rh => {
                let name = match seen.recall.server {
                    Some(name) => name,
                    None => ENTRY_NAME.to_owned(),
                };
                RecallHome::Connected(name)
            }
            Ok(seen) if seen.recall.taken => recall_elsewhere(&text),
            Ok(_) => RecallHome::Absent,
        },
    }
}

fn recall_elsewhere(text: &str) -> RecallHome {
    let docs = match serde_yaml_ng::from_str::<Vec<Value>>(text) {
        Ok(docs) => docs,
        Err(error) => return RecallHome::Unread(error.to_string()),
    };
    for row in rows_of(&docs) {
        if row_runs_rh(&row) || !names_our_recall(&row) {
            continue;
        }
        let invocation = row
            .recall
            .as_ref()
            .map(|fields| {
                let mut text = fields.command.clone();
                for arg in &fields.args {
                    text.push(' ');
                    text.push_str(arg);
                }
                text
            })
            .unwrap_or_default();
        return RecallHome::Elsewhere {
            name: row_server(&row),
            invocation,
        };
    }
    RecallHome::Absent
}

fn capture_gap(paths: &Paths, profiles: &[String]) -> String {
    gap(
        &format!("no rh plug-in in {}", paths.patch.display()),
        "plug-in linked in ",
        paths,
        profiles,
        legacy_capture,
    )
}

fn recall_gap(paths: &Paths, profiles: &[String]) -> String {
    gap(
        &format!("no rh mcp entry in {}", paths.patch.display()),
        "rh mcp in ",
        paths,
        profiles,
        legacy_recall,
    )
}

fn gap(
    none: &str,
    linked: &str,
    paths: &Paths,
    profiles: &[String],
    wired: fn(&Path, &str) -> bool,
) -> String {
    if profiles.is_empty() {
        return format!("{none}; no profile directory yet");
    }
    let (yes, no) = split_profiles(&paths.root, profiles, wired);
    if yes.is_empty() {
        format!("{none}; not wired: {}", no.join(", "))
    } else if no.is_empty() {
        format!(
            "{linked}{}; profiles created later are not covered",
            yes.join(", ")
        )
    } else {
        format!("{linked}{}; not wired: {}", yes.join(", "), no.join(", "))
    }
}

fn cover_detail(patch: &Path, profiles: &[String]) -> String {
    if profiles.is_empty() {
        format!(
            "home patch {} covers every profile, including profiles created later",
            patch.display()
        )
    } else {
        format!(
            "home patch {} covers every profile ({}), including profiles created later",
            patch.display(),
            profiles.join(", ")
        )
    }
}

fn split_profiles(
    root: &Path,
    profiles: &[String],
    wired: fn(&Path, &str) -> bool,
) -> (Vec<String>, Vec<String>) {
    let mut yes = Vec::new();
    let mut no = Vec::new();
    for name in profiles {
        if wired(root, name) {
            yes.push(name.clone());
        } else {
            no.push(name.clone());
        }
    }
    (yes, no)
}
