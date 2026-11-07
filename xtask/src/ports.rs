use std::path::{Component, Path};

use syn::{Attribute, ItemTrait, Visibility};

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
                    "PORTS {}: testkit must be a dev-dependency; database access belongs to kernel ports → docs/PATTERNS.md#ports",
                    package.name
                ));
            }
        }
    }
}

pub(crate) fn inspect(rel: &str, item: &ItemTrait, hits: &mut Hits) {
    if scoped(rel) && matches!(item.vis, Visibility::Public(_)) && !waived(&item.attrs) {
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
                | "terminal"
                | "memory-postgres"
                | "memory-sqlite"
                | "pki"
        )
    )
}

fn waived(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        matches!(attr.style, syn::AttrStyle::Outer)
            && attr.path().is_ident("expect")
            && crate::ast::reason(attr.meta.clone()).is_some_and(|reason| {
                reason
                    .split(|character: char| !character.is_alphanumeric() && character != '_')
                    .any(|word| word == "PORTS")
            })
    })
}
