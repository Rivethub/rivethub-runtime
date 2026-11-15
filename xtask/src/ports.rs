use std::path::{Component, Path};

use syn::{ItemTrait, Visibility};

use crate::baseline::Baseline;
use crate::error::Result;
use crate::rustsrc::Hits;

pub fn check(root: &Path, baseline: &Baseline, findings: &mut Vec<String>) -> Result<()> {
    crate::rustsrc::check(root, baseline, "PORTS", findings)
}

pub fn testkit_dependencies(metadata: &cargo_metadata::Metadata, findings: &mut Vec<String>) {
    for package in &metadata.packages {
        for dependency in &package.dependencies {
            if dependency.name == "testkit"
                && dependency.kind != cargo_metadata::DependencyKind::Development
            {
                findings.push(format!(
                    "PORTS {}: testkit must be a dev-dependency; database access belongs to kernel ports",
                    package.name
                ));
            }
        }
    }
}

pub(crate) fn inspect(rel: &str, item: &ItemTrait, hits: &mut Hits) {
    if scoped(rel) && matches!(item.vis, Visibility::Public(_)) {
        hits.push(
            "PORTS",
            item.ident.span(),
            format!("pub trait {}", item.ident),
        );
    }
}

fn scoped(rel: &str) -> bool {
    let mut parts = Path::new(rel).components();
    if parts.next() != Some(Component::Normal("crates".as_ref())) {
        return false;
    }
    let Some(Component::Normal(name)) = parts.next() else {
        return false;
    };
    matches!(
        name.to_str(),
        Some(
            "gateway"
                | "workers"
                | "mcp"
                | "capture"
                | "providers"
                | "harness-acp"
                | "memory-postgres"
                | "memory-sqlite"
        )
    )
}
