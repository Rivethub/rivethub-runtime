use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct PolicyFile {
    lints: LintsPolicy,
}

#[derive(Debug, Deserialize)]
struct LintsPolicy {
    #[serde(default)]
    rust: Option<toml::Table>,
    #[serde(default)]
    clippy: Option<toml::Table>,
}

const BINARY_CRATE: &str = "rivethub";
const MIGRATIONS_CRATE: &str = "migrate";

#[must_use]
fn allowed_migrate_dependency(name: &str) -> bool {
    matches!(name, "wire" | "config")
}

pub fn binary_dependency(metadata: &cargo_metadata::Metadata, findings: &mut Vec<String>) {
    for package in &metadata.packages {
        let depends = package.dependencies.iter().any(|dependency| {
            dependency.name == BINARY_CRATE || dependency.rename.as_deref() == Some(BINARY_CRATE)
        });
        if depends {
            findings.push(format!(
                "MANIFEST {}: workspace members must not depend on the {BINARY_CRATE} binary",
                package.name
            ));
        }
    }
}

pub fn migrations_leave_the_runtime(
    metadata: &cargo_metadata::Metadata,
    findings: &mut Vec<String>,
) {
    let members: BTreeSet<&str> = metadata
        .packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for package in &metadata.packages {
        let next = package
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind == cargo_metadata::DependencyKind::Normal)
            .filter(|dependency| package.name != "cli" || dependency.name != MIGRATIONS_CRATE)
            .filter(|dependency| members.contains(dependency.name.as_str()))
            .map(|dependency| dependency.name.as_str())
            .collect();
        edges.insert(package.name.as_str(), next);
    }
    if reaches(&edges, BINARY_CRATE, MIGRATIONS_CRATE) {
        findings.push(format!(
            "MANIFEST {BINARY_CRATE}: must not depend on the {MIGRATIONS_CRATE} crate except through cli's direct dependency for schema verification — schema changes are applied only by rivethub-migrate, outside the runtime (dev-dependencies stay legal)"
        ));
    }
    for package in &metadata.packages {
        if package.name != MIGRATIONS_CRATE {
            continue;
        }
        for dependency in &package.dependencies {
            let name = dependency.name.as_str();
            if dependency.kind == cargo_metadata::DependencyKind::Normal
                && members.contains(name)
                && !allowed_migrate_dependency(name)
            {
                findings.push(format!(
                    "MANIFEST {MIGRATIONS_CRATE}: depends on {name} — the migrations crate may depend only on wire, config and external crates (dev-dependencies stay legal)"
                ));
            }
        }
    }
}

fn reaches(edges: &BTreeMap<&str, Vec<&str>>, from: &str, target: &str) -> bool {
    let mut seen: BTreeSet<&str> = BTreeSet::from([from]);
    let mut queue = vec![from];
    while let Some(current) = queue.pop() {
        for next in edges.get(current).into_iter().flatten() {
            if *next == target {
                return true;
            }
            if seen.insert(next) {
                queue.push(next);
            }
        }
    }
    false
}

pub fn manifests(root: &Path, findings: &mut Vec<String>) -> Result<Vec<String>> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let mut names = Vec::new();
    for package in &metadata.packages {
        let source = package.manifest_path.strip_prefix(root).map_err(|_| {
            Error::Finding(format!(
                "MANIFEST {}: manifest {} is outside the workspace root",
                package.name, package.manifest_path
            ))
        })?;
        let manifest_path = root.join(source);
        let name = package.name.to_string();
        if name == "xtask" {
            names.push(name);
            continue;
        }
        names.push(name.clone());
        member(&manifest_path, &name, findings)?;
    }
    binary_dependency(&metadata, findings);
    migrations_leave_the_runtime(&metadata, findings);
    crate::ports::testkit_dependencies(&metadata, findings);
    Ok(names)
}

fn member(manifest_path: &Path, name: &str, findings: &mut Vec<String>) -> Result<()> {
    let Some(dir) = manifest_path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|parent| parent.to_str())
    else {
        findings.push(format!(
            "MANIFEST {name}: manifest path has no parent directory"
        ));
        return Ok(());
    };
    if name != dir {
        findings.push(format!(
            "MANIFEST {name}: package.name must equal the directory name"
        ));
    }
    let text = std::fs::read_to_string(manifest_path)?;
    let value: toml::Table = toml::from_str(&text)?;
    let Some(package_table) = value.get("package").and_then(toml::Value::as_table) else {
        findings.push(format!("MANIFEST {name}: manifest has no [package] table"));
        return Ok(());
    };
    let lints = value.get("lints");
    let workspace_flag = lints.and_then(|lints| lints.get("workspace"));
    let workspace_lints = workspace_flag.is_some_and(toml::Value::is_bool)
        && workspace_flag.and_then(toml::Value::as_bool) == Some(true);
    if !workspace_lints {
        findings.push(format!("MANIFEST {name}: missing [lints] workspace = true"));
    }
    for key in ["edition", "license", "version", "publish", "rust-version"] {
        let inherited = package_table
            .get(key)
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true);
        if !inherited {
            findings.push(format!(
                "MANIFEST {name}: package.{key} must be workspace = true"
            ));
        }
    }
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(entries) = value.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for (dep, spec) in entries {
            if spec
                .as_table()
                .and_then(|table| table.get("path"))
                .is_some()
            {
                continue;
            }
            let from_workspace = spec
                .as_table()
                .and_then(|table| table.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            if !from_workspace {
                findings.push(format!(
                    "MANIFEST {name}: {section}.{dep} must come from [workspace.dependencies]"
                ));
            }
        }
    }
    Ok(())
}

pub fn lint_policy(root: &Path, findings: &mut Vec<String>) -> Result<()> {
    let text = std::fs::read_to_string(root.join("xtask").join("lints-policy.toml"))?;
    let expected: PolicyFile = toml::from_str(&text)?;
    let actual_text = std::fs::read_to_string(root.join("Cargo.toml"))?;
    let actual_value: toml::Table = toml::from_str(&actual_text)?;
    let workspace = actual_value
        .get("workspace")
        .and_then(toml::Value::as_table);
    let lints = workspace.and_then(|table| table.get("lints"));
    let actual: Option<LintsPolicy> = lints.cloned().map(LintsPolicy::deserialize).transpose()?;
    let policy_matches = actual.as_ref().is_some_and(|actual| {
        table_eq(actual.rust.as_ref(), expected.lints.rust.as_ref())
            && table_eq(actual.clippy.as_ref(), expected.lints.clippy.as_ref())
    });
    if !policy_matches {
        findings.push(
            "LINTPOLICY [workspace.lints] differs from xtask/lints-policy.toml (lint policy changes only in a reviewed commit)"
                .to_owned(),
        );
    }
    Ok(())
}

fn table_eq(actual: Option<&toml::Table>, expected: Option<&toml::Table>) -> bool {
    let normalize = |table: Option<&toml::Table>| -> Vec<(String, String)> {
        table
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), render(value)))
            .collect()
    };
    normalize(actual) == normalize(expected)
}

fn render(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Table(table) => {
            let mut parts: Vec<String> = table
                .iter()
                .map(|(key, value)| format!("{key}={}", render(value)))
                .collect();
            parts.sort();
            format!("{{{}}}", parts.join(","))
        }
        toml::Value::Integer(value) => value.to_string(),
        toml::Value::Float(value) => value.to_string(),
        toml::Value::Boolean(value) => value.to_string(),
        toml::Value::Datetime(value) => value.to_string(),
        toml::Value::Array(values) => {
            let parts: Vec<String> = values.iter().map(render).collect();
            format!("[{}]", parts.join(","))
        }
    }
}
