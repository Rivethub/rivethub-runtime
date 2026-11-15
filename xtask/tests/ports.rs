use std::path::Path;

use tempfile::TempDir;
use xtask::{baseline, git, rustsrc};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn repository(files: &[(&str, &str)]) -> Result<TempDir> {
    let repo = tempfile::tempdir()?;
    git::git(repo.path(), &["init", "-q"])?;
    for (rel, source) in files {
        let path = repo.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, source)?;
    }
    git::git(repo.path(), &["add", "."])?;
    Ok(repo)
}

fn findings(root: &Path) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    rustsrc::check(
        root,
        &baseline::Baseline::load(root)?,
        "PORTS",
        &mut findings,
    )?;
    Ok(findings)
}

#[test]
fn public_traits_fail_in_every_service_and_adapter() -> Result {
    for name in [
        "gateway",
        "workers",
        "mcp",
        "capture",
        "providers",
        "harness-acp",
        "memory-postgres",
        "memory-sqlite",
    ] {
        let file = format!("crates/{name}/src/lib.rs");
        let repo = repository(&[(&file, "pub trait Store: Send + Sync {}")])?;
        assert_eq!(
            findings(repo.path())?,
            [format!("PORTS {file}:1: pub trait Store (occurrence 1)")]
        );
    }
    Ok(())
}

#[test]
fn kernel_foundation_composition_and_similar_names_pass() -> Result {
    for name in [
        "kernel",
        "wire",
        "config",
        "testkit",
        "rivethub",
        "cli",
        "gateway-extra",
    ] {
        let file = format!("crates/{name}/src/lib.rs");
        let repo = repository(&[(&file, "pub trait Store {}")])?;
        assert!(findings(repo.path())?.is_empty(), "{name}");
    }
    Ok(())
}

#[test]
fn restricted_traits_and_non_declarations_pass() -> Result {
    let repo = repository(&[(
        "crates/gateway/src/lib.rs",
        r#"
trait Private {}
pub(crate) trait Internal {}
mod nested {
    pub(super) trait Parent {}
    pub(in crate::nested) trait Local {}
}
const TEXT: &str = "pub trait Fake {}";
impl kernel::Store for Adapter {}
"#,
    )])?;
    assert_eq!(findings(repo.path())?, Vec::<String>::new());
    Ok(())
}

#[test]
fn nested_traits_and_attributes_do_not_hide_ports() -> Result {
    let repo = repository(&[(
        "crates/gateway/src/lib.rs",
        r"
mod private {
    #[async_trait]
    pub trait Runner { async fn run(&self); }
}
#[cfg(test)]
mod tests { pub trait Store {} }
items! { pub trait External {} }
",
    )])?;
    let findings = findings(repo.path())?;
    assert_eq!(findings.len(), 3);
    for name in ["Runner", "Store", "External"] {
        assert!(findings.iter().any(|finding| finding.contains(name)));
    }
    Ok(())
}

#[test]
fn the_generated_baseline_counts_duplicates_and_passes_the_gate() -> Result {
    let file = "crates/capture/src/lib.rs";
    let source = "mod first { pub trait Store {} } mod second { pub trait Store {} }";
    let repo = repository(&[(file, source), (baseline::FILE, "")])?;
    let hits = rustsrc::collect(repo.path())?;
    baseline::write_hits(repo.path(), hits)?;
    let saved = baseline::Baseline::load(repo.path())?;
    assert_eq!(
        saved.entries(),
        &[baseline::Entry {
            check: "PORTS".into(),
            file: file.into(),
            text: "pub trait Store".into(),
            count: 2,
        }]
    );
    let mut invalid = Vec::new();
    baseline::validate(&saved, &mut invalid);
    assert_eq!(invalid, Vec::<String>::new());
    assert_eq!(findings(repo.path())?, Vec::<String>::new());
    Ok(())
}
