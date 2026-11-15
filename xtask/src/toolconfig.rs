use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use serde::Deserialize;

use crate::error::Result;
use crate::git;

#[derive(Debug, Clone, Copy, strum::Display)]
enum ToolFile {
    #[strum(serialize = "clippy.toml")]
    ClippyToml,
    #[strum(serialize = ".clippy.toml")]
    HiddenClippyToml,
    #[strum(serialize = "rustfmt.toml")]
    RustfmtToml,
    #[strum(serialize = ".rustfmt.toml")]
    HiddenRustfmtToml,
    #[strum(serialize = "deny.toml")]
    DenyToml,
    #[strum(serialize = ".cargo/config.toml")]
    CargoConfigToml,
    #[strum(serialize = "rust-toolchain")]
    RustToolchain,
    #[strum(serialize = "rust-toolchain.toml")]
    RustToolchainToml,
}

impl ToolFile {
    const ALL: [Self; 8] = [
        Self::ClippyToml,
        Self::HiddenClippyToml,
        Self::RustfmtToml,
        Self::HiddenRustfmtToml,
        Self::DenyToml,
        Self::CargoConfigToml,
        Self::RustToolchain,
        Self::RustToolchainToml,
    ];

    fn matches(self, rel: &str) -> bool {
        let name = self.to_string();
        if matches!(self, Self::CargoConfigToml) {
            return rel == name || rel.ends_with(&format!("/{name}"));
        }
        Path::new(rel)
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|file| file == name)
    }
}

#[derive(Debug, Default, Deserialize)]
struct WorkspaceTable {
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RootManifest {
    #[serde(default)]
    workspace: WorkspaceTable,
}

#[derive(Debug, Deserialize)]
struct Member {
    lints: Option<Lints>,
}

#[derive(Debug, Deserialize)]
struct Lints {
    #[serde(default)]
    workspace: bool,
    #[serde(flatten)]
    other: BTreeMap<String, toml::Value>,
}

pub fn check(root: &Path, findings: &mut Vec<String>) -> Result<()> {
    let excluded = excluded_roots(root)?;
    let mut members = Vec::new();
    for rel in git::tracked_files(root)? {
        let dir = owning_dir(&rel);
        if own_workspace(dir, &excluded) {
            continue;
        }
        for tool in ToolFile::ALL {
            if tool.matches(&rel) {
                findings.push(format!(
                    "TOOLCONFIG {rel}: {tool} must live only at the workspace root"
                ));
            }
        }
        if rel.ends_with("/Cargo.toml") {
            members.push(rel);
        }
    }
    for member in &members {
        member_lints(root, member, findings)?;
    }
    Ok(())
}

fn owning_dir(rel: &str) -> &str {
    let path = Path::new(rel);
    if path.file_name() == Some(OsStr::new("config.toml"))
        && path.parent().and_then(Path::file_name) == Some(OsStr::new(".cargo"))
    {
        return path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::to_str)
            .unwrap_or_default();
    }
    parent_of(rel)
}

fn excluded_roots(root: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let manifest: RootManifest = toml::from_str(&text)?;
    Ok(manifest.workspace.exclude)
}

fn parent_of(rel: &str) -> &str {
    Path::new(rel)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
}

fn own_workspace(dir: &str, excluded: &[String]) -> bool {
    dir.is_empty() || excluded.iter().any(|root| root == dir)
}

fn member_lints(root: &Path, rel: &str, findings: &mut Vec<String>) -> Result<()> {
    let text = std::fs::read_to_string(root.join(rel))?;
    let member: Member = toml::from_str(&text)?;
    match member.lints {
        Some(Lints {
            workspace: true,
            other,
        }) => {
            for tool in other.into_keys() {
                findings.push(format!(
                    "TOOLCONFIG {rel}: own [lints.{tool}] table (lints come from [workspace.lints])"
                ));
            }
        }
        _ => findings.push(format!(
            "TOOLCONFIG {rel}: missing [lints] workspace = true"
        )),
    }
    Ok(())
}
