use std::path::Path;

use tempfile::TempDir;
use xtask::{baseline, git, ports, rustsrc};

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
    ports::check(root, &baseline::Baseline::load(root)?, &mut findings)?;
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
        "terminal",
        "memory-postgres",
        "memory-sqlite",
    ] {
        let file = format!("crates/{name}/src/lib.rs");
        let repo = repository(&[(&file, "pub trait Store: Send + Sync {}")])?;
        assert_eq!(
            findings(repo.path())?,
            [format!(
                "PORTS {file}:1: pub trait Store (occurrence 1) → docs/PATTERNS.md#ports"
            )]
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
fn sealed_extension_with_local_pattern_reason_passes() -> Result {
    let repo = repository(&[(
        "crates/providers/src/lib.rs",
        r#"
mod sealed { pub(crate) trait Sealed {} }
#[expect(private_bounds, reason = "PORTS: sealed extension helper, no external boundary")]
pub trait Extension: sealed::Sealed {}
"#,
    )])?;
    assert_eq!(findings(repo.path())?, Vec::<String>::new());
    Ok(())
}

#[test]
fn unrelated_missing_invalid_and_conditional_reasons_fail() -> Result {
    for attribute in [
        "#[expect(dead_code)]",
        "#[expect(dead_code, reason = \"internal helper\")]",
        "#[expect(dead_code, reason = \"TRANSPORTS\")]",
        "#[expect(dead_code, reason = \"PORTS_EXTRA\")]",
        "#[expect(dead_code, reason = 123)]",
        "#[allow(dead_code, reason = \"PORTS\")]",
        "#[cfg_attr(any(), expect(dead_code, reason = \"PORTS\"))]",
    ] {
        let source = format!("{attribute}\npub trait Store {{}}");
        let repo = repository(&[("crates/gateway/src/lib.rs", &source)])?;
        assert_eq!(findings(repo.path())?.len(), 1, "{attribute}");
    }
    Ok(())
}

#[test]
fn expectations_do_not_leak_to_siblings_or_from_modules() -> Result {
    let repo = repository(&[(
        "crates/gateway/src/lib.rs",
        r#"
#[expect(dead_code, reason = "PORTS: extension")]
pub trait Extension {}
pub trait Store {}
#[expect(dead_code, reason = "PORTS: module")]
mod nested { pub trait Runner {} }
"#,
    )])?;
    assert_eq!(findings(repo.path())?.len(), 2);
    Ok(())
}

#[test]
fn malformed_source_is_an_error() -> Result {
    let repo = repository(&[("crates/gateway/src/lib.rs", "pub trait Store {")])?;
    assert!(findings(repo.path()).is_err());
    Ok(())
}

#[test]
fn generated_baseline_counts_duplicates_and_rejects_growth_and_staleness() -> Result {
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
    std::fs::write(
        repo.path().join(file),
        format!("{source} mod third {{ pub trait Store {{}} }}"),
    )?;
    let grown = findings(repo.path())?;
    assert_eq!(grown.len(), 1);
    assert!(grown.iter().all(|finding| finding.contains("occurrence 3")));
    std::fs::write(repo.path().join(file), "pub trait Store {}")?;
    let stale = findings(repo.path())?;
    assert_eq!(stale.len(), 1);
    assert!(
        stale
            .iter()
            .all(|finding| finding.contains("no longer matches code"))
    );
    Ok(())
}

#[test]
fn ports_baseline_cannot_grow_against_main() {
    let entry = baseline::Entry {
        check: "PORTS".into(),
        file: "crates/capture/src/writer.rs".into(),
        text: "pub trait CaptureTransport".into(),
        count: 1,
    };
    let mut findings = Vec::new();
    baseline::compare(&[], std::slice::from_ref(&entry), &mut findings);
    assert_eq!(findings.len(), 1);
    findings.clear();
    let mut increased = entry.clone();
    increased.count = 2;
    baseline::compare(std::slice::from_ref(&entry), &[increased], &mut findings);
    assert_eq!(findings.len(), 1);
    findings.clear();
    baseline::compare(&[entry], &[], &mut findings);
    assert_eq!(findings, Vec::<String>::new());
}

#[test]
fn workspace_ports_match_the_generated_baseline() -> Result {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask must live under the workspace root")?;
    assert_eq!(findings(root)?, Vec::<String>::new());
    Ok(())
}
