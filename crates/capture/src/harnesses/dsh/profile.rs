use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use serde_yaml_ng::Value;

use super::super::plan::read_optional;
use super::CAPTURE_ID;
use super::MARKER;
use super::PATCH_NAME;
use super::patch::Row;
use super::patch::names_our_plugin;
use super::patch::names_our_recall;
use super::patch::not_array;
use super::patch::row_runs_rh;
use super::patch::row_server;
use super::patch::rows_of;

#[derive(Debug, Default)]
pub(super) struct CaptureScan {
    pub(super) marker: bool,
    pub(super) named: bool,
    pub(super) missing: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub(super) struct RecallScan {
    pub(super) runs_rh: bool,
    pub(super) taken: bool,
    pub(super) server: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Seen {
    pub(super) capture: CaptureScan,
    pub(super) recall: RecallScan,
}

pub(super) fn seen_in(root: &Path, text: &str) -> Result<Seen, String> {
    let mut seen = Seen::default();
    if text.trim().is_empty() {
        return Ok(seen);
    }
    let docs: Vec<Value> = serde_yaml_ng::from_str(text).map_err(not_array)?;
    for row in rows_of(&docs) {
        note_row(root, &row, &mut seen);
    }
    Ok(seen)
}

fn note_row(root: &Path, row: &Row, seen: &mut Seen) {
    if names_our_plugin(&row.name) {
        seen.capture.named = true;
    }
    match resolved_plugin(root, &row.name) {
        Some(path) if contains_marker(&path) => seen.capture.marker = true,
        Some(path) if row.id == CAPTURE_ID || names_our_plugin(&row.name) => {
            if seen.capture.missing.is_none() && !path.exists() {
                seen.capture.missing = Some(path);
            }
        }
        Some(_) | None => {}
    }
    if row_runs_rh(row) {
        seen.recall.runs_rh = true;
        if seen.recall.server.is_none() {
            seen.recall.server = Some(row_server(row));
        }
        return;
    }
    if names_our_recall(row) {
        seen.recall.taken = true;
    }
}

pub(super) fn profile_names(root: &Path) -> Vec<String> {
    let entries = match fs::read_dir(root.join("profiles")) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(%error, "dsh profiles");
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && path.join("package.json").is_file()
            && let Some(name) = entry.file_name().to_str().map(str::to_owned)
        {
            names.push(name);
        }
    }
    names.sort();
    names
}

pub(super) fn legacy_capture(root: &Path, name: &str) -> bool {
    let dir = root.join("profiles").join(name);
    package_capture(&dir) || patch_mentions(&dir, true)
}

pub(super) fn legacy_recall(root: &Path, name: &str) -> bool {
    patch_mentions(&root.join("profiles").join(name), false)
}

#[serde_as]
#[derive(Deserialize)]
struct ProfilePackage {
    #[serde(default)]
    #[serde_as(as = "BTreeMap<_, DefaultOnError>")]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    #[serde_as(as = "BTreeMap<_, DefaultOnError>")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dsh: Option<ProfileDsh>,
}

#[derive(Deserialize)]
struct ProfileDsh {
    #[serde(default)]
    profile: Option<ProfileBundles>,
}

#[derive(Deserialize)]
struct ProfileBundles {
    #[serde(default)]
    bundles: Vec<String>,
}

fn package_capture(dir: &Path) -> bool {
    let text = match fs::read_to_string(dir.join("package.json")) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "dsh profile package");
            return false;
        }
    };
    let package = match serde_json::from_str::<ProfilePackage>(&text) {
        Ok(package) => package,
        Err(error) => {
            tracing::debug!(%error, "dsh profile package");
            return false;
        }
    };
    let bundles = package
        .dsh
        .and_then(|section| section.profile)
        .map(|profile| profile.bundles)
        .unwrap_or_default();
    package
        .dependencies
        .values()
        .chain(package.dev_dependencies.values())
        .any(|spec| link_has_marker(dir, spec))
        || bundles
            .iter()
            .any(|name| contains_marker(&dir.join("node_modules").join(name)))
}

fn link_has_marker(dir: &Path, spec: &str) -> bool {
    let Some(raw) = spec
        .strip_prefix("link:")
        .or_else(|| spec.strip_prefix("file:"))
    else {
        return false;
    };
    let raw = raw.strip_prefix("file://").unwrap_or(raw);
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    contains_marker(&path)
}

fn patch_mentions(dir: &Path, capture: bool) -> bool {
    let text = match read_optional(&dir.join(PATCH_NAME)) {
        Ok(Some(text)) => text,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!(%error, "dsh profile patch");
            return false;
        }
    };
    let docs = match serde_yaml_ng::from_str::<Vec<Value>>(&text) {
        Ok(docs) => docs,
        Err(error) => {
            tracing::debug!(%error, "dsh profile patch");
            return false;
        }
    };
    rows_of(&docs).iter().any(|row| {
        if capture {
            resolved_plugin(dir, &row.name).is_some_and(|path| contains_marker(&path))
                || bare_module(dir, &row.name).is_some_and(|path| contains_marker(&path))
        } else {
            row_runs_rh(row)
        }
    })
}

fn bare_module(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.starts_with('.') || name.starts_with('/') || name.starts_with("file:") {
        return None;
    }
    Some(dir.join("node_modules").join(name))
}

fn resolved_plugin(base: &Path, name: &str) -> Option<PathBuf> {
    if let Some(path) = file_url(name) {
        return Some(path);
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if name.starts_with('.') {
        return Some(base.join(path));
    }
    None
}

fn file_url(name: &str) -> Option<PathBuf> {
    let rest = name.strip_prefix("file://")?;
    let path = rest.strip_prefix("//localhost").unwrap_or(rest);
    if path.starts_with("//") {
        return None;
    }
    Some(PathBuf::from(percent_decode(path)))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes.get(index) == Some(&b'%')
            && let Some(hex) = bytes.get(index + 1..index + 3)
            && let Some(value) = hex_byte(hex)
        {
            out.push(value);
            index = index.saturating_add(3);
            continue;
        }
        if let Some(byte) = bytes.get(index) {
            out.push(*byte);
        }
        index = index.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_byte(bytes: &[u8]) -> Option<u8> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            tracing::debug!(%error, "dsh file url");
            return None;
        }
    };
    match u8::from_str_radix(text, 16) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::debug!(%error, "dsh file url");
            None
        }
    }
}

fn contains_marker(path: &Path) -> bool {
    let files = if path.is_dir() {
        vec![path.join("index.mjs"), path.join("index.js")]
    } else {
        vec![path.to_path_buf()]
    };
    files
        .iter()
        .any(|file| fs::read_to_string(file).is_ok_and(|text| text.contains(MARKER)))
}
