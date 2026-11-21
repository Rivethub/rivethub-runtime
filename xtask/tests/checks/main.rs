use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use xtask::authorship;
use xtask::baseline;
use xtask::check;
use xtask::code;
use xtask::contract;
use xtask::dupfixture;
use xtask::labpath;
use xtask::lang;
use xtask::manifests;
use xtask::privacy;
use xtask::toolconfig;

fn scan_findings(
    check: &str,
    files: &[(&str, &str)],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    for (rel, body) in files {
        repo.write(rel, body)?;
    }
    repo.track()?;
    let baseline = baseline::Baseline::load(repo.root())?;
    let mut findings = Vec::new();
    xtask::rustsrc::check(repo.root(), &baseline, check, &mut findings)?;
    Ok(findings)
}

struct Repo {
    root: PathBuf,
    _guard: TempDir,
}

impl Repo {
    fn new() -> std::io::Result<Self> {
        let guard = tempfile::tempdir()?;
        let root = guard.path().to_path_buf();
        git(&root, &["init", "-q"])?;
        Ok(Self {
            root,
            _guard: guard,
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, rel: &str, content: &str) -> std::io::Result<()> {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }

    fn track(&self) -> std::io::Result<()> {
        git(&self.root, &["add", "-A"]).map(|_| ())
    }
}

fn git(root: &Path, args: &[&str]) -> std::io::Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_CONFIG_COUNT", "3")
        .env("GIT_CONFIG_KEY_0", "user.name")
        .env("GIT_CONFIG_VALUE_0", "Test User")
        .env("GIT_CONFIG_KEY_1", "user.email")
        .env("GIT_CONFIG_VALUE_1", "test@example.com")
        .env("GIT_CONFIG_KEY_2", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_2", "target/git-hooks")
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

const ALLOW: &str = r#"
extensions = ["md", "rs", "toml"]
paths = ["LICENSE", "coverage-floor.txt"]
globs = ["crates/*/tests/golden/*.json", "xtask/data/*.json"]
"#;

fn seed_languages(repo: &Repo) -> Result<(), Box<dyn std::error::Error>> {
    repo.write("xtask/languages.toml", ALLOW)?;
    repo.write("README.md", "runtime\n")?;
    Ok(())
}

#[test]
fn allow_list_passes() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    seed_languages(&repo)?;
    repo.write("crates/wire/src/lib.rs", "pub fn start() {}\n")?;
    repo.write("crates/wire/Cargo.toml", "[package]\nname = \"wire\"\n")?;
    repo.write("crates/wire/tests/golden/echo.json", "{}\n")?;
    repo.write("LICENSE", "Apache-2.0\n")?;
    repo.write("coverage-floor.txt", "0\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    lang::check(repo.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "the allow-list must pass, got: {findings:?}"
    );
    Ok(())
}

#[test]
fn second_language_fails() -> Result<(), Box<dyn std::error::Error>> {
    for (file, body) in [
        ("tools/hook.ts", "export const a = 1\n"),
        ("scripts/plugin.py", "print('x')\n"),
        ("scripts/run.sh", "echo x\n"),
        ("package.json", "{}\n"),
        ("crates/wire/src/extra.js", "var a = 1\n"),
    ] {
        let repo = Repo::new()?;
        seed_languages(&repo)?;
        repo.write(file, body)?;
        repo.track()?;
        let mut findings = Vec::new();
        lang::check(repo.root(), &mut findings)?;
        assert!(
            findings.iter().any(|finding| finding.contains(file)),
            "{file} must fail the allow-list, got: {findings:?}"
        );
    }
    Ok(())
}

#[test]
fn installed_hooks_reject_and_accept_commits() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    seed_languages(&repo)?;
    repo.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
    )?;
    repo.write(
        "crates/wire/Cargo.toml",
        "[package]\nname = \"wire\"\nedition = \"2021\"\nversion = \"0.1.0\"\n",
    )?;
    repo.write("crates/wire/src/lib.rs", "pub fn start() {}\n")?;
    repo.write("xtask/data/privacy-denylist.json", "{\"sha256\": []}")?;
    repo.write("xtask/data/secret-denylist.json", "{\"sha256\": []}")?;
    repo.track()?;
    let hook_binary = std::env::vars_os()
        .find(|(name, _)| name == std::ffi::OsStr::new("CARGO_BIN_EXE_xtask"))
        .map(|(_, value)| value)
        .ok_or("the xtask binary is missing (CARGO_BIN_EXE_xtask)")?;
    check::copy_hooks(repo.root(), Path::new(&hook_binary))?;
    assert!(repo.root().join("target/git-hooks/pre-commit").exists());
    assert!(repo.root().join("target/git-hooks/commit-msg").exists());

    let bad = git(
        repo.root(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "build: x\n\nCo-authored-by: Claude <noreply@anthropic.com>\n",
        ],
    );
    assert!(
        bad.is_err(),
        "an AI co-author trailer must be rejected by the installed commit-msg hook"
    );

    let good = git(
        repo.root(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "build: install repository hooks",
        ],
    );
    assert!(
        good.is_ok(),
        "a person's commit must pass the installed hooks: {good:?}"
    );
    Ok(())
}

#[test]
fn comment_fires_and_passes() {
    let bad = "// note\npub fn a() {}\n";
    let mut findings = Vec::new();
    code::check_source(&mut findings, "crates/mcp/src/lib.rs", bad);
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("COMMENT ")),
        "a comment must be reported, got: {findings:?}"
    );

    let good = "pub fn a() {}\n";
    let mut findings = Vec::new();
    code::check_source(&mut findings, "crates/mcp/src/lib.rs", good);
    assert!(findings.is_empty(), "clean code must pass: {findings:?}");

    let strings =
        "pub const A: &str = \"// not a comment\";\npub fn b() -> bool { a().contains(\"/*\") }\n";
    let mut findings = Vec::new();
    code::check_source(&mut findings, "crates/mcp/src/lib.rs", strings);
    assert!(
        findings.is_empty(),
        "comment marks inside string literals must pass: {findings:?}"
    );
}

#[test]
fn size_ceiling_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    let mut big = String::new();
    for index in 0..610 {
        big.push_str("pub fn item_");
        big.push_str(&index.to_string());
        big.push_str("() {}\n\n");
    }
    repo.write("crates/mcp/src/big.rs", &big)?;
    repo.write("crates/mcp/src/small.rs", "pub fn a() {}\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    code::check_code(repo.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("SIZE crates/mcp/src/big.rs")),
        "a 610-line file must exceed the ceiling, got: {findings:?}"
    );
    assert!(
        !findings.iter().any(|finding| finding.contains("small.rs")),
        "small files pass: {findings:?}"
    );
    Ok(())
}

#[test]
fn naming_file_names_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    repo.write("crates/mcp/src/den.rs", "pub fn a() {}\n")?;
    repo.write("crates/mcp/src/gateway.rs", "pub fn a() {}\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    code::check_code(repo.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("NAMING crates/mcp/src/den.rs")),
        "a den.rs file name must be reported, got: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("gateway.rs")),
        "gateway.rs passes: {findings:?}"
    );
    Ok(())
}

fn config_repo(repo: &Repo) -> Result<(), Box<dyn std::error::Error>> {
    repo.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"lints\"]\n",
    )?;
    repo.write("clippy.toml", "allow-unwrap-in-tests = true\n")?;
    repo.write("rustfmt.toml", "edition = \"2024\"\n")?;
    repo.write("deny.toml", "[graph]\nall-features = true\n")?;
    repo.write(
        ".cargo/config.toml",
        "[alias]\nxtask = \"run -q -p xtask --\"\n",
    )?;
    repo.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.0\"\n")?;
    Ok(())
}

#[test]
fn toolconfig_root_only_files_pass() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write("crates/wire/Cargo.toml", &manifest("wire", ""))?;
    repo.write("crates/wire/src/lib.rs", "")?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "tool files at the workspace root pass: {findings:?}"
    );
    Ok(())
}

#[test]
fn toolconfig_nested_clippy_toml_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write("crates/wire/Cargo.toml", &manifest("wire", ""))?;
    repo.write("crates/wire/clippy.toml", "disallowed-types = []\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
    assert!(
        findings
            .first()
            .is_some_and(|finding| finding.contains("crates/wire/clippy.toml")),
        "the finding names the file: {findings:?}"
    );
    Ok(())
}

#[test]
fn toolconfig_member_without_lints_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write("crates/wire/Cargo.toml", "[package]\nname = \"wire\"\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
    assert!(
        findings.first().is_some_and(|finding| {
            finding.contains("crates/wire/Cargo.toml")
                && finding.contains("missing [lints] workspace = true")
        }),
        "the finding names the manifest: {findings:?}"
    );
    Ok(())
}

#[test]
fn toolconfig_member_own_lints_table_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write(
        "crates/wire/Cargo.toml",
        "[package]\nname = \"wire\"\n\n[lints]\nworkspace = true\n\n[lints.clippy]\nunwrap_used = \"deny\"\n",
    )?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
    assert!(
        findings
            .first()
            .is_some_and(|finding| finding.contains("own [lints.clippy] table")),
        "the finding names the table: {findings:?}"
    );
    Ok(())
}

#[test]
fn toolconfig_excluded_directory_is_its_own_root() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write("lints/Cargo.toml", "[package]\nname = \"rr_lints\"\n")?;
    repo.write(
        "lints/rust-toolchain.toml",
        "[toolchain]\nchannel = \"nightly\"\n",
    )?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "an excluded workspace root owns its tool files: {findings:?}"
    );
    Ok(())
}

#[test]
fn toolconfig_nested_below_excluded_root_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    config_repo(&repo)?;
    repo.write("lints/inner/clippy.toml", "disallowed-types = []\n")?;
    repo.track()?;
    let mut findings = Vec::new();
    toolconfig::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 1, "exactly one finding: {findings:?}");
    assert!(
        findings
            .first()
            .is_some_and(|finding| finding.contains("lints/inner/clippy.toml")),
        "the finding names the file: {findings:?}"
    );
    Ok(())
}

fn manifest(crate_name: &str, extra: &str) -> String {
    format!(
        "[package]\nname = \"{crate_name}\"\nedition.workspace = true\nlicense.workspace = true\n\
         version.workspace = true\npublish.workspace = true\nrust-version.workspace = true\n\
         repository.workspace = true\n{extra}\n[lints]\nworkspace = true\n"
    )
}

fn workspace_toml() -> String {
    "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
     [workspace.package]\nedition = \"2024\"\nlicense = \"Apache-2.0\"\npublish = false\n\
     version = \"0.1.0\"\nrust-version = \"1.99\"\n\
     repository = \"https://example.com/rivethub-runtime\"\n\n\
     [workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n\
     [workspace.dependencies]\nthiserror = \"2\"\n"
        .to_owned()
}

#[test]
fn manifest_inheritance_is_enforced() -> Result<(), Box<dyn std::error::Error>> {
    let good = Repo::new()?;
    good.write("Cargo.toml", &workspace_toml())?;
    good.write(
        "crates/wire/Cargo.toml",
        &manifest("wire", "[dependencies]\nthiserror.workspace = true\n"),
    )?;
    good.write("crates/wire/src/lib.rs", "")?;
    good.track()?;
    let mut findings = Vec::new();
    manifests::manifests(good.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "inherited manifests pass: {findings:?}"
    );

    let bad = Repo::new()?;
    bad.write("Cargo.toml", &workspace_toml())?;
    bad.write(
        "crates/wire/Cargo.toml",
        &manifest("misnamed", "[dependencies]\nthiserror = \"2\"\n")
            .replace("[lints]\nworkspace = true\n", ""),
    )?;
    bad.write("crates/wire/src/lib.rs", "")?;
    bad.track()?;
    let mut findings = Vec::new();
    manifests::manifests(bad.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("package.name must equal the directory name")),
        "a misnamed package is reported: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("missing [lints] workspace = true")),
        "a crate without [lints] workspace is reported: {findings:?}"
    );
    assert!(
        findings.iter().any(|finding| finding
            .contains("dependencies.thiserror must come from [workspace.dependencies]")),
        "an out-of-workspace dependency is reported: {findings:?}"
    );
    Ok(())
}

#[test]
fn manifest_member_depending_on_the_binary_fires() -> Result<(), Box<dyn std::error::Error>> {
    let bad = Repo::new()?;
    bad.write("Cargo.toml", &workspace_toml())?;
    bad.write("crates/rivethub/Cargo.toml", &manifest("rivethub", ""))?;
    bad.write("crates/rivethub/src/main.rs", "fn main() {}\n")?;
    bad.write(
        "crates/cli/Cargo.toml",
        &manifest(
            "cli",
            "[dependencies]\nrivethub = { path = \"../rivethub\" }\n",
        ),
    )?;
    bad.write("crates/cli/src/lib.rs", "")?;
    bad.track()?;
    let mut findings = Vec::new();
    manifests::manifests(bad.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("MANIFEST cli")
                && finding.contains("must not depend on the rivethub binary")),
        "the member linking the binary is reported: {findings:?}"
    );

    let good = Repo::new()?;
    good.write("Cargo.toml", &workspace_toml())?;
    good.write("crates/rivethub/Cargo.toml", &manifest("rivethub", ""))?;
    good.write("crates/rivethub/src/main.rs", "fn main() {}\n")?;
    good.write("crates/gateway/Cargo.toml", &manifest("gateway", ""))?;
    good.write("crates/gateway/src/lib.rs", "")?;
    good.track()?;
    let mut findings = Vec::new();
    manifests::manifests(good.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "a member free of the binary passes: {findings:?}"
    );
    Ok(())
}

#[test]
fn manifest_rivethub_must_not_reach_the_migrate_crate() -> Result<(), Box<dyn std::error::Error>> {
    let transitively = Repo::new()?;
    transitively.write("Cargo.toml", &workspace_toml())?;
    transitively.write("crates/migrate/Cargo.toml", &manifest("migrate", ""))?;
    transitively.write("crates/migrate/src/lib.rs", "")?;
    transitively.write(
        "crates/gateway/Cargo.toml",
        &manifest(
            "gateway",
            "[dependencies]\nmigrate = { path = \"../migrate\" }\n",
        ),
    )?;
    transitively.write("crates/gateway/src/lib.rs", "")?;
    transitively.write(
        "crates/rivethub/Cargo.toml",
        &manifest(
            "rivethub",
            "[dependencies]\ngateway = { path = \"../gateway\" }\n",
        ),
    )?;
    transitively.write("crates/rivethub/src/main.rs", "fn main() {}\n")?;
    transitively.track()?;
    let mut findings = Vec::new();
    manifests::manifests(transitively.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("MANIFEST rivethub")
                && finding.contains("must not depend on the migrate crate")),
        "a transitive normal dependency on migrate is reported: {findings:?}"
    );

    let direct = Repo::new()?;
    direct.write("Cargo.toml", &workspace_toml())?;
    direct.write("crates/migrate/Cargo.toml", &manifest("migrate", ""))?;
    direct.write("crates/migrate/src/lib.rs", "")?;
    direct.write(
        "crates/rivethub/Cargo.toml",
        &manifest(
            "rivethub",
            "[dependencies]\nmigrate = { path = \"../migrate\" }\n",
        ),
    )?;
    direct.write("crates/rivethub/src/main.rs", "fn main() {}\n")?;
    direct.track()?;
    let mut findings = Vec::new();
    manifests::manifests(direct.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("MANIFEST rivethub")
                && finding.contains("must not depend on the migrate crate")),
        "a direct normal dependency on migrate is reported: {findings:?}"
    );

    let dev_only = Repo::new()?;
    dev_only.write("Cargo.toml", &workspace_toml())?;
    dev_only.write("crates/migrate/Cargo.toml", &manifest("migrate", ""))?;
    dev_only.write("crates/migrate/src/lib.rs", "")?;
    dev_only.write(
        "crates/rivethub/Cargo.toml",
        &manifest(
            "rivethub",
            "[dev-dependencies]\nmigrate = { path = \"../migrate\" }\n",
        ),
    )?;
    dev_only.write("crates/rivethub/src/main.rs", "fn main() {}\n")?;
    dev_only.track()?;
    let mut findings = Vec::new();
    manifests::manifests(dev_only.root(), &mut findings)?;
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("must not depend on the migrate crate")),
        "a dev-dependency on migrate stays legal: {findings:?}"
    );
    Ok(())
}

#[test]
fn manifest_migrate_depends_only_downward() -> Result<(), Box<dyn std::error::Error>> {
    let upward = Repo::new()?;
    upward.write("Cargo.toml", &workspace_toml())?;
    upward.write(
        "crates/memory-postgres/Cargo.toml",
        &manifest("memory-postgres", ""),
    )?;
    upward.write("crates/memory-postgres/src/lib.rs", "")?;
    upward.write(
        "crates/migrate/Cargo.toml",
        &manifest(
            "migrate",
            "[dependencies]\nmemory-postgres = { path = \"../memory-postgres\" }\n",
        ),
    )?;
    upward.write("crates/migrate/src/lib.rs", "")?;
    upward.track()?;
    let mut findings = Vec::new();
    manifests::manifests(upward.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("MANIFEST migrate")
                && finding.contains("may depend only on wire, config and external crates")),
        "a normal dependency from migrate on an adapter is reported: {findings:?}"
    );

    let dev_reach = Repo::new()?;
    dev_reach.write("Cargo.toml", &workspace_toml())?;
    dev_reach.write(
        "crates/memory-postgres/Cargo.toml",
        &manifest("memory-postgres", ""),
    )?;
    dev_reach.write("crates/memory-postgres/src/lib.rs", "")?;
    dev_reach.write(
        "crates/migrate/Cargo.toml",
        &manifest(
            "migrate",
            "[dev-dependencies]\nmemory-postgres = { path = \"../memory-postgres\" }\n",
        ),
    )?;
    dev_reach.write("crates/migrate/src/lib.rs", "")?;
    dev_reach.track()?;
    let mut findings = Vec::new();
    manifests::manifests(dev_reach.root(), &mut findings)?;
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("may depend only on wire, config and external crates")),
        "migrate's dev-dependencies on the adapters stay legal: {findings:?}"
    );
    Ok(())
}

#[test]
fn lint_policy_fires_and_passes() -> Result<(), Box<dyn std::error::Error>> {
    let policy =
        "[lints.rust]\nunsafe_code = \"forbid\"\n\n[lints.clippy]\nunwrap_used = \"deny\"\n";
    let repo = Repo::new()?;
    repo.write("xtask/lints-policy.toml", policy)?;
    repo.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = []\n\n[workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n[workspace.lints.clippy]\nunwrap_used = \"deny\"\n",
    )?;
    let mut findings = Vec::new();
    manifests::lint_policy(repo.root(), &mut findings)?;
    assert!(findings.is_empty(), "matching policy passes: {findings:?}");

    repo.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = []\n\n[workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n[workspace.lints.clippy]\nunwrap_used = \"allow\"\n",
    )?;
    let mut findings = Vec::new();
    manifests::lint_policy(repo.root(), &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("LINTPOLICY ")),
        "a lint change outside the policy file fails: {findings:?}"
    );
    Ok(())
}

fn authorship(text: &str, author: Option<(&str, &str)>) -> Vec<String> {
    let mut findings = Vec::new();
    authorship::commit_message(&mut findings, text, author);
    findings
}

#[test]
fn any_person_may_author_and_be_credited() {
    for author in [
        ("Foreign Dev", "dev@foreign.example"),
        ("Claude Dupont", "claude@dupont.example"),
        ("octocat", "583231+octocat@users.noreply.github.com"),
    ] {
        let findings = authorship("fix: move the cursor to the row end\n", Some(author));
        assert!(findings.is_empty(), "{author:?} passes: {findings:?}");
    }
    let findings = authorship(
        "fix: move the cursor to the row end\n\nCo-authored-by: Ada Lovelace <ada@example.org>\nSigned-off-by: Ada Lovelace <ada@example.org>\n",
        None,
    );
    assert!(findings.is_empty(), "a person is credited: {findings:?}");
    for text in [
        "docs: explain the pages generated with mdbook\n\nThe cursor sessions table keeps its session_id column.\n",
        "fix(capture): advance cursor after reading session_part rows\n",
        "fix(capture): read Grok sessions\n\nSession keys are\ngrok-build:<uuid> and event ids are <session>:l<line>.\n",
        "feat(capture): import the sessions created by Claude Code and Cursor\n",
        "docs: follow https://docs.anthropic.com/en/api/messages for the request shape\n",
        "feat(capture): import transcripts\n\nNormalize logs\ncreated by Claude Code and Cursor before indexing.\n",
        "fix(capture): parse SessionStart\n\nSee https://docs.claude.com/en/docs/claude-code/hooks#sessionstart\n",
        "fix(capture): align prefixes\n\nClaude-hook: emits tool_use.\nCursor-based: paginates by token.\n",
        "docs: list the fixtures\n\n- Generated by Claude Code 2.1\n- Generated by Codex 0.9\n",
        "fix: x\n\nReviewed-by: Claude Dupont <claude@dupont.example>\nCo-authored-by: Claude Dupont <claude@dupont.example>\n",
    ] {
        let findings = authorship(text, None);
        assert!(
            findings.is_empty(),
            "{text:?} is ordinary wording: {findings:?}"
        );
    }
}

#[test]
fn ai_products_may_not_author_or_be_credited() {
    for author in [
        ("Claude", "noreply@anthropic.com"),
        ("Cursor Agent", "cursoragent@cursor.com"),
        ("Copilot", "198982749+Copilot@users.noreply.github.com"),
        ("Claude Opus 5.5", "someone@example.org"),
        ("Gemini 2.5 Pro", "someone@example.org"),
        ("Claude 3.7 Sonnet", "bot@example.org"),
        ("OpenCode Bot", "bot@example.org"),
        ("Alice Smith (aider)", "alice@example.org"),
        (
            "factory-droid[bot]",
            "138933559+factory-droid[bot]@users.noreply.github.com",
        ),
    ] {
        let findings = authorship("fix: a change\n", Some(author));
        assert!(
            findings
                .iter()
                .any(|finding| finding.contains("is an AI product")),
            "{author:?} fails: {findings:?}"
        );
    }
    for text in [
        "fix: x\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n",
        "fix: x\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\n",
        "fix: x\n\nClaude-Session: https://claude.ai/code/session_01\n",
        "fix: x\n\nGenerated with [Claude Code](https://claude.com/claude-code)\n",
        "fix: x\n\nAmp-Thread-ID: https://ampcode.com/threads/T-1\n",
        "fix: x\r\n\r\nCo-authored-by: Claude Opus 4.6 <noreply@anthropic.com>\r\n",
        "fix: x\n\nMade-with: Cursor\n",
        "fix: x\n\nGenerated by Codex\n",
        "fix: x\n\nClaude-Session: 01abc\n",
        "fix: x\n\nclaude-session: 01abc\n",
        "fix: x\n\nClaude-Session:\t01abc\n",
        "fix: x\n\nMade-with: cursor\n",
        "fix: x\n\nGenerated with https://claude.ai/code\n",
        "fix: x\n\nAssisted-by: Claude Code\n",
        "fix: x\n\nSigned-off-by: Claude <noreply@anthropic.com>\n",
        "fix: x\n\nCo-Authored-By: Warp <agent@warp.dev>\n",
        "fix: x\n\nCo-authored-by: openhands <openhands@all-hands.dev>\n",
    ] {
        let findings = authorship(text, None);
        assert!(
            findings
                .iter()
                .any(|finding| finding.starts_with("AUTHORSHIP ")),
            "{text:?} fails: {findings:?}"
        );
    }
}

#[test]
fn commits_mode_reports_range() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    repo.track()?;
    let output = std::process::Command::new("git")
        .current_dir(repo.root())
        .args([
            "-c",
            "user.name=Test User",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: seeded",
        ])
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .output()?;
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut findings = Vec::new();
    let count = authorship::commits(repo.root(), "HEAD", &mut findings)?;
    assert_eq!(count, 1);
    assert!(findings.is_empty(), "a person's commits pass: {findings:?}");

    let output = std::process::Command::new("git")
        .current_dir(repo.root())
        .args([
            "-c",
            "user.name=Test User",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: committed by a tool",
        ])
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env("GIT_COMMITTER_NAME", "Test User (aider)")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()?;
    assert!(output.status.success());
    let mut findings = Vec::new();
    authorship::commits(repo.root(), "HEAD~1..HEAD", &mut findings)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("Test User (aider)")),
        "an AI committer fails: {findings:?}"
    );
    Ok(())
}

#[test]
fn dupfixture_cross_crate_duplicate_fires() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    repo.write(
        "crates/wire/tests/golden/capture/writer/results.json",
        "{\"single\":{\"ok\":true}}\n",
    )?;
    repo.write(
        "crates/testkit/tests/fixtures/capture/writer/results.json",
        "{\"single\":{\"ok\":true}}\n",
    )?;
    repo.write(
        "crates/wire/tests/golden/capture/claude-hook/hook-ts.json",
        "{\"entries\":[]}\n",
    )?;
    repo.write(
        "crates/capture/tests/golden/capture/claude-hook/hook-ts.json",
        "{\"entries\":[]}\n",
    )?;
    repo.track()?;
    let mut findings = Vec::new();
    dupfixture::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 2, "both pairs fire: {findings:?}");
    assert!(
        findings
            .iter()
            .all(|finding| finding.starts_with("DUPFIXTURE ")),
        "findings carry the DUPFIXTURE label: {findings:?}"
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains("crates/testkit/tests/fixtures/capture/writer/results.json")
                && finding.contains("crates/wire/tests/golden/capture/writer/results.json")
        }),
        "the writer pair is named: {findings:?}"
    );
    assert!(
        findings.iter().any(|finding| {
            finding.contains("crates/capture/tests/golden/capture/claude-hook/hook-ts.json")
                && finding.contains("crates/wire/tests/golden/capture/claude-hook/hook-ts.json")
        }),
        "the claude-hook pair is named: {findings:?}"
    );
    Ok(())
}

#[test]
fn dupfixture_one_crate_copies_pass() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    let chunks = "{\"chunks\":[]}\n";
    repo.write(
        "crates/wire/tests/golden/providers/vllm/A-text/chunks.json",
        chunks,
    )?;
    repo.write(
        "crates/wire/tests/golden/providers/ollama/A-text/chunks.json",
        chunks,
    )?;
    repo.write(
        "crates/wire/tests/fixtures/provider/A-text/chunks.json",
        chunks,
    )?;
    repo.write("crates/wire/tests/golden/DEVIATIONS.md", "# deviations\n")?;
    repo.write(
        "crates/config/tests/golden/DEVIATIONS.md",
        "# other deviations\n",
    )?;
    repo.track()?;
    let mut findings = Vec::new();
    dupfixture::check(repo.root(), &mut findings)?;
    assert!(
        findings.is_empty(),
        "identical files inside one crate and differing files across crates pass: {findings:?}"
    );
    Ok(())
}

#[test]
fn labpath_fires_and_passes() -> Result<(), Box<dyn std::error::Error>> {
    let needle = concat!("/rivet-shared", "/plans/rust-rewrite/golden");
    let repo = Repo::new()?;
    repo.write(
        "crates/memory-postgres/tests/capture_pg.rs",
        &format!("const GOLDEN_PG: &str = \"{needle}\";\n"),
    )?;
    repo.write(
        "xtask/src/notes.md",
        &format!("re-record from {needle}/capture/pg\n"),
    )?;
    repo.write(
        "crates/config/src/paths.rs",
        "const DEFAULT_SHARED_DIR: &str = \"/rivet-shared\";\n",
    )?;
    repo.write(
        "crates/wire/tests/golden/gateway/http-tls.json",
        &format!("{{\"note\": \"the inventory ({needle}/inventory/gateway-api.md)\"}}\n"),
    )?;
    repo.write(
        "crates/testkit/tests/fixtures/capture/pg/README.md",
        &format!("re-record from {needle}/capture/pg\n"),
    )?;
    repo.write("docs/CONTRACT.md", &format!("reference: {needle}/\n"))?;
    repo.track()?;
    let mut findings = Vec::new();
    let count = labpath::check(repo.root(), &mut findings)?;
    assert_eq!(count, 2, "one crates/ and one xtask/ finding: {findings:?}");
    assert!(
        findings.iter().any(
            |finding| finding.starts_with("LABPATH crates/memory-postgres/tests/capture_pg.rs")
        ),
        "a test naming the plans tree is reported: {findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("LABPATH xtask/src/notes.md")),
        "an xtask file naming the plans tree is reported: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("crates/config/src/paths.rs")),
        "`/rivet-shared` on its own stays legal: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("tests/golden/")
                || finding.contains("tests/fixtures/")),
        "recorded data keeps its recorded bytes: {findings:?}"
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding.starts_with("LABPATH docs/")),
        "files outside crates/ and xtask/ are not scanned: {findings:?}"
    );
    Ok(())
}

#[test]
fn denylisted_value_is_reported() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    let uuid = "0b6e10c2-c155-bbb6-f9ea-9141ccaca9aa";
    let digest = privacy::sha256(uuid);
    repo.write(
        "xtask/data/privacy-denylist.json",
        &format!("{{\"sha256\": [\"{digest}\"]}}"),
    )?;
    repo.write("xtask/data/secret-denylist.json", "{\"sha256\": []}")?;
    repo.track()?;
    let mut findings = Vec::new();
    let priv_deny = privacy::load_hashes(repo.root(), "privacy-denylist.json", &mut findings)?;
    let sec_deny = privacy::load_hashes(repo.root(), "secret-denylist.json", &mut findings)?;

    let empty: HashSet<String> = HashSet::new();
    let mut findings = Vec::new();
    privacy::scan_text(
        &mut findings,
        &format!("let id = \"{uuid}\";\n"),
        "crates/wire/src/lib.rs",
        &empty,
        &empty,
        None,
    );
    assert!(findings.is_empty(), "empty denylists report nothing");

    let mut findings = Vec::new();
    privacy::scan_text(
        &mut findings,
        &format!("let id = \"{uuid}\";\n"),
        "crates/wire/src/lib.rs",
        &priv_deny,
        &sec_deny,
        None,
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("denylist-uuid")),
        "a denylisted uuid is reported: {findings:?}"
    );

    let mut findings = Vec::new();
    privacy::scan_text(
        &mut findings,
        "let address = \"192.168.1.20\";\n", // gate-allow: the payload is the finding
        "crates/wire/src/lib.rs",
        &priv_deny,
        &sec_deny,
        None,
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("private-ip")),
        "a private address is reported: {findings:?}"
    );

    let mut findings = Vec::new();
    privacy::scan_text(
        &mut findings,
        "curl -H \"Authorization: Bearer sk-abcdefghijklmnopqrstuvwx\"\n", // gate-allow: the payload is the finding
        "crates/wire/src/lib.rs",
        &priv_deny,
        &sec_deny,
        None,
    );
    assert!(
        findings.iter().any(|finding| finding.contains("secret")),
        "an API key shape is reported: {findings:?}"
    );

    let mut findings = Vec::new();
    privacy::scan_text(
        &mut findings,
        &format!("see /home/someone/notes\n{uuid}\n"),
        "docs/notes.md",
        &priv_deny,
        &sec_deny,
        None,
    );
    assert!(
        findings.iter().any(|finding| finding.contains("home-path")),
        "a foreign home path in a marker file is reported: {findings:?}"
    );
    Ok(())
}

#[test]
fn skip_scan_paths() {
    assert!(privacy::skip_scan("Cargo.lock"));
    assert!(privacy::skip_scan("xtask/data/privacy-denylist.json"));
    assert!(privacy::skip_scan("crates/wire/logo.png"));
    assert!(!privacy::skip_scan("crates/wire/src/lib.rs"));
}

#[test]
fn env_scan_fires_outside_config() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "ENV",
        &[(
            "crates/gateway/src/auth.rs",
            "pub fn probe() -> String {\n    std::env::var(\"RIVETHUB_HOST\").unwrap_or_default()\n}\n",
        )],
    )?;
    assert!(
        findings.iter().any(
            |finding| finding.starts_with("ENV crates/gateway/src/auth.rs")
                && finding.contains("env::var")
        ),
        "an env read outside config is reported: {findings:?}"
    );

    let findings = scan_findings(
        "ENV",
        &[
            (
                "crates/config/src/environment.rs",
                "pub fn load() -> Environment {\n    let vars = std::env::vars_os();\n    let dir = std::env::temp_dir();\n    let home = std::env::home_dir();\n    let cwd = std::env::current_dir();\n    drop((vars, dir, home, cwd));\n}\n",
            ),
            (
                "crates/rivethub/src/main.rs",
                "fn main() {\n    let vars = std::env::vars_os();\n    drop(vars);\n}\n",
            ),
        ],
    )?;
    assert!(
        findings.is_empty(),
        "config and the rivethub main are exempt: {findings:?}"
    );
    Ok(())
}

#[test]
fn privilege_scan_fires_on_fail_open_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "PRIVILEGE",
        &[(
            "crates/gateway/src/auth.rs",
            "fn role(spec: &Spec) -> Role {\n    spec.role.unwrap_or(Role::Admin)\n}\n\nfn viewer(spec: &Spec) -> Viewer {\n    spec.viewer.unwrap_or(Viewer::System)\n}\n\nfn owner(spec: &Spec) -> Role {\n    spec.role.unwrap_or(Role::Owner)\n}\n\nfn permission(spec: &Spec) -> Permission {\n    spec.permission.unwrap_or_default()\n}\n",
        )],
    )?;
    for needle in [
        "unwrap_or(..Admin..)",
        "unwrap_or(..System..)",
        "unwrap_or(..Owner..)",
        "unwrap_or_default() on permission",
    ] {
        assert!(
            findings.iter().any(|finding| finding.contains(needle)),
            "{needle} is reported: {findings:?}"
        );
    }

    let findings = scan_findings(
        "PRIVILEGE",
        &[(
            "crates/gateway/src/auth.rs",
            "fn scope(args: &Args) -> MemoryScope {\n    args.scope.unwrap_or(MemoryScope::Both)\n}\n\nfn count(args: &Args) -> usize {\n    args.count.unwrap_or_default()\n}\n",
        )],
    )?;
    assert!(
        findings.is_empty(),
        "a non-privilege default passes: {findings:?}"
    );
    Ok(())
}

#[test]
fn pathcontain_scan_fires_on_string_containment() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "PATHCONTAIN",
        &[(
            "crates/capture/src/spool.rs",
            "pub fn inside(path: &Path, root: &Path) -> bool {\n    path.to_string_lossy().starts_with(root.to_string_lossy().to_string().as_str())\n}\n\npub fn inside_str(path: &Path, root: &Path) -> bool {\n    path.to_str().unwrap_or_default().starts_with(\"/srv\")\n}\n\npub fn inside_display(path: &Path, root: &Path) -> bool {\n    path.display().to_string().starts_with(\"/srv\")\n}\n",
        )],
    )?;
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.starts_with("PATHCONTAIN"))
            .count(),
        3,
        "all three string containments are reported: {findings:?}"
    );

    let findings = scan_findings(
        "PATHCONTAIN",
        &[(
            "crates/xtask-check/src/lib.rs",
            "pub fn inside(path: &Path, root: &Path) -> bool {\n    path.starts_with(root)\n}\n\npub fn label(rel: &str) -> bool {\n    rel.starts_with(\"crates/\")\n}\n",
        )],
    )?;
    assert!(
        findings.is_empty(),
        "Path::starts_with and plain prefixes pass: {findings:?}"
    );
    Ok(())
}

#[test]
fn waiver_scan_fires_on_inner_attributes() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "WAIVER",
        &[(
            "crates/demo/src/lib.rs",
            "#![expect(dead_code)]\n\npub fn start() {}\n",
        )],
    )?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("WAIVER crates/demo/src/lib.rs")),
        "a file-wide expect is reported: {findings:?}"
    );

    let findings = scan_findings(
        "WAIVER",
        &[(
            "crates/demo/src/lib.rs",
            "#![allow(dead_code)]\n\npub fn start() {}\n",
        )],
    )?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("WAIVER crates/demo/src/lib.rs")),
        "a file-wide allow is reported: {findings:?}"
    );

    let findings = scan_findings(
        "WAIVER",
        &[(
            "crates/demo/src/lib.rs",
            "#[expect(clippy::manual_div_ceil)]\npub fn start() {}\n",
        )],
    )?;
    assert!(
        findings.is_empty(),
        "an item-level expectation passes: {findings:?}"
    );
    Ok(())
}

#[test]
fn utf16_scan_fires_outside_wire() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "UTF16",
        &[(
            "crates/capture/src/writer.rs",
            "pub fn cap(text: &str, max: usize) -> String {\n    let mut units = 0usize;\n    let _ = text.chars().map(char::len_utf16).sum::<usize>();\n    String::new()\n}\n",
        )],
    )?;
    assert!(
        findings.iter().any(|finding| finding.contains("len_utf16")),
        "len_utf16 is reported: {findings:?}"
    );

    let findings = scan_findings(
        "UTF16",
        &[
            (
                "crates/wire/src/cap.rs",
                "pub fn cap(text: &str) -> usize {\n    text.chars().map(char::len_utf16).sum()\n}\n",
            ),
            (
                "crates/capture/src/event_id.rs",
                "pub fn width(text: &str) -> usize {\n    text.chars().map(char::len_utf16).sum()\n}\n",
            ),
        ],
    )?;
    assert!(
        findings.is_empty(),
        "wire and the capture event-id hash are exempt: {findings:?}"
    );
    Ok(())
}

#[test]
fn jsmodel_scan_fires_on_js_names() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "JSMODEL",
        &[(
            "crates/testkit/src/echo.rs",
            "pub fn js_number(value: u64) -> u64 {\n    value\n}\n\npub struct JsJson;\n",
        )],
    )?;
    assert_eq!(findings.len(), 2, "both names are reported: {findings:?}");
    assert!(
        findings.iter().any(|finding| finding.contains("js_number"))
            && findings.iter().any(|finding| finding.contains("JsJson")),
        "js_ functions and Js[A-Z] types are reported: {findings:?}"
    );

    let findings = scan_findings(
        "JSMODEL",
        &[(
            "crates/testkit/src/echo.rs",
            "pub const JSON_LIMIT: usize = 4;\n\npub fn parse_json(text: &str) -> Json {\n    Json\n}\n\npub struct JsonValue;\n",
        )],
    )?;
    assert!(findings.is_empty(), "Json names pass: {findings:?}");

    let findings = scan_findings(
        "JSMODEL",
        &[(
            "crates/capture/src/event_id.rs",
            "pub fn hash(value: JsJson) -> String {\n    String::new()\n}\n",
        )],
    )?;
    assert!(
        findings.is_empty(),
        "the capture event-id hash module is exempt: {findings:?}"
    );
    Ok(())
}

#[test]
fn jsonbytes_scan_fires_outside_wire() -> Result<(), Box<dyn std::error::Error>> {
    let findings = scan_findings(
        "JSONBYTES",
        &[(
            "crates/testkit/src/scan.rs",
            "pub fn scan(bytes: &[u8]) -> usize {\n    let mut depth = 0usize;\n    for byte in bytes {\n        match byte {\n            b'{' => depth += 1,\n            b'}' => depth -= 1,\n            b'\"' => {}\n            _ => {}\n        }\n    }\n    depth\n}\n",
        )],
    )?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("JSONBYTES crates/testkit/src/scan.rs")),
        "a hand-rolled JSON byte scanner is reported: {findings:?}"
    );

    let findings = scan_findings(
        "JSONBYTES",
        &[(
            "crates/wire/src/scan.rs",
            "pub fn scan(bytes: &[u8]) -> usize {\n    let mut depth = 0usize;\n    for byte in bytes {\n        if byte == b'\"' {\n            depth += 1;\n        }\n    }\n    depth\n}\n",
        )],
    )?;
    assert!(findings.is_empty(), "wire is exempt: {findings:?}");

    let findings = scan_findings(
        "JSONBYTES",
        &[(
            "crates/testkit/src/scan.rs",
            "pub fn scan(bytes: &[u8]) -> usize {\n    bytes.iter().filter(|byte| **byte == b'\"').count()\n}\n",
        )],
    )?;
    assert!(
        findings.is_empty(),
        "a lone byte literal is no scanner: {findings:?}"
    );
    Ok(())
}

#[test]
fn baseline_counts_duplicate_hits_and_staleness() {
    let repo = Repo::new().unwrap();
    repo.write("xtask/baseline.toml", "[[entry]]\ncheck = \"UTF16\"\nfile = \"crates/sample/src/lib.rs\"\ntext = \"len_utf16\"\ncount = 1\n").unwrap();
    let baseline = baseline::Baseline::load(repo.root()).unwrap();
    let hit = xtask::rustsrc::Hit {
        file: "crates/sample/src/lib.rs".into(),
        line: 2,
        text: "len_utf16".into(),
    };
    let mut findings = Vec::new();
    baseline::adjudicate(
        &baseline,
        "UTF16",
        std::slice::from_ref(&hit),
        &mut findings,
    );
    assert!(findings.is_empty(), "{findings:?}");
    baseline::adjudicate(&baseline, "UTF16", &[hit.clone(), hit], &mut findings);
    assert_eq!(findings.len(), 1);
    assert!(
        findings
            .first()
            .is_some_and(|finding| finding.contains("occurrence 2"))
    );
    findings.clear();
    baseline::adjudicate(&baseline, "UTF16", &[], &mut findings);
    assert!(
        findings
            .first()
            .is_some_and(|finding| finding.contains("no longer matches code"))
    );
}

#[test]
fn baseline_may_only_shrink_against_origin_main() {
    let repo = Repo::new().unwrap();
    let seed = "[[entry]]\ncheck = \"FALLBACK\"\nfile = \"crates/gateway/src/extract.rs\"\ntext = \"unwrap_or_default\"\ncount = 1\n";
    repo.write("xtask/baseline.toml", seed).unwrap();
    repo.track().unwrap();
    git(
        repo.root(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "build: seed the baseline",
        ],
    )
    .unwrap();
    git(
        repo.root(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    )
    .unwrap();

    let mut findings = Vec::new();
    baseline::shrink(repo.root(), &mut findings).unwrap();
    assert!(
        findings.is_empty(),
        "the merge-base set passes: {findings:?}"
    );

    repo.write(
        "xtask/baseline.toml",
        "[[entry]]\ncheck = \"FALLBACK\"\nfile = \"crates/gateway/src/extract.rs\"\ntext = \"unwrap_or_default\"\ncount = 1\n\n[[entry]]\ncheck = \"FALLBACK\"\nfile = \"crates/gateway/src/auth.rs\"\ntext = \"unwrap_or\"\ncount = 1\n",
    )
    .unwrap();
    let mut findings = Vec::new();
    baseline::shrink(repo.root(), &mut findings).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("crates/gateway/src/auth.rs")
                && finding.contains("may only shrink")),
        "an added entry fails: {findings:?}"
    );

    repo.write("xtask/baseline.toml", "").unwrap();
    let mut findings = Vec::new();
    baseline::shrink(repo.root(), &mut findings).unwrap();
    assert!(
        findings.is_empty(),
        "a shrunk baseline passes: {findings:?}"
    );
}

#[test]
fn the_landing_commit_sets_the_baseline_floor() {
    let repo = Repo::new().unwrap();
    repo.write("README.md", "runtime\n").unwrap();
    repo.track().unwrap();
    git(
        repo.root(),
        &["commit", "-q", "--allow-empty", "-m", "build: seed"],
    )
    .unwrap();
    git(
        repo.root(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    )
    .unwrap();
    repo.write(
        "xtask/baseline.toml",
        "[[entry]]\ncheck = \"FALLBACK\"\nfile = \"crates/gateway/src/extract.rs\"\ntext = \"unwrap_or_default\"\ncount = 1\n",
    )
    .unwrap();

    let mut findings = Vec::new();
    baseline::shrink(repo.root(), &mut findings).unwrap();
    assert!(
        findings.is_empty(),
        "the commit that introduces the baseline sets the floor: {findings:?}"
    );
}

#[test]
fn non_utf8_source_is_a_finding() {
    let repo = Repo::new().unwrap();
    let path = repo.root().join("crates/mcp/src/binary.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, [0x70, 0x75, 0x62, 0x20, 0xff, 0x0a]).unwrap();
    repo.track().unwrap();
    let mut findings = Vec::new();
    code::check_code(repo.root(), &mut findings).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.starts_with("UTF8 crates/mcp/src/binary.rs")),
        "a non-UTF-8 source file is reported: {findings:?}"
    );
}

#[test]
fn baseline_counts_cannot_grow_even_when_the_key_is_unchanged() {
    let old = baseline::Entry {
        check: "UTF16".into(),
        file: "crates/sample/src/lib.rs".into(),
        text: "len_utf16".into(),
        count: 2,
    };
    let mut current = old.clone();
    let mut findings = Vec::new();
    current.count = 3;
    baseline::compare(
        std::slice::from_ref(&old),
        std::slice::from_ref(&current),
        &mut findings,
    );
    assert_eq!(findings.len(), 1);
    current.count = 1;
    findings.clear();
    baseline::compare(&[old], &[current], &mut findings);
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn baseline_duplicate_and_zero_count_entries_are_invalid() {
    let repo = Repo::new().unwrap();
    let entry = "[[entry]]\ncheck = \"UTF16\"\nfile = \"crates/sample/src/lib.rs\"\ntext = \"len_utf16\"\ncount = 0\n";
    repo.write("xtask/baseline.toml", &entry.repeat(2)).unwrap();
    let baseline = baseline::Baseline::load(repo.root()).unwrap();
    let mut findings = Vec::new();
    baseline::validate(&baseline, &mut findings);
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("positive count"))
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("duplicate entry"))
    );
}

#[test]
fn ast_distinguishes_syntax_from_raw_strings_and_nested_attributes()
-> Result<(), Box<dyn std::error::Error>> {
    let clean = scan_findings(
        "JSMODEL",
        &[(
            "crates/sample/src/lib.rs",
            "const TEXT: &str = r###\"fn js_fake() {} struct JsFake;\"###;",
        )],
    )?;
    assert!(clean.is_empty(), "{clean:?}");
    let raw = scan_findings(
        "JSMODEL",
        &[("crates/sample/src/lib.rs", "fn r#js_real() {}")],
    )?;
    assert_eq!(raw.len(), 1);
    let inner = scan_findings(
        "WAIVER",
        &[(
            "crates/sample/src/lib.rs",
            "mod nested { #![cfg_attr(test, expect(dead_code))] fn item() {} }",
        )],
    )?;
    assert_eq!(inner.len(), 1);
    Ok(())
}

#[test]
fn malformed_rust_fails_instead_of_being_skipped() {
    let result = scan_findings("UTF16", &[("crates/sample/src/lib.rs", "fn incomplete(")]);
    assert!(result.is_err());
}

#[test]
fn importing_an_environment_reader_is_still_a_read_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let hits = scan_findings(
        "ENV",
        &[(
            "crates/sample/src/lib.rs",
            "use std::env::{var as read, current_dir}; fn load() { let _ = read(\"RIVETHUB_HOST\"); }",
        )],
    )?;
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn a_second_ast_occurrence_in_the_same_file_exceeds_the_baseline()
-> Result<(), Box<dyn std::error::Error>> {
    let hits = scan_findings(
        "UTF16",
        &[
            (
                "xtask/baseline.toml",
                "[[entry]]\ncheck = \"UTF16\"\nfile = \"crates/sample/src/lib.rs\"\ntext = \"len_utf16\"\ncount = 1\n",
            ),
            (
                "crates/sample/src/lib.rs",
                "fn width(c: char) { let _ = c.len_utf16(); let _ = c.len_utf16(); }",
            ),
        ],
    )?;
    assert_eq!(hits.len(), 1);
    assert!(hits.first().is_some_and(|hit| hit.contains("occurrence 2")));
    Ok(())
}

#[test]
fn contract_fires_on_missing_schema_and_missing_root() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repo::new()?;
    repo.write(
        "crates/wire/src/contract.rs",
        "const ROOTS: &[Root] = &[SchemaGenerator::subschema_for::<crate::events::Listed>];\n",
    )?;
    repo.write(
        "crates/wire/src/events.rs",
        "#[derive(Serialize, schemars::JsonSchema)]\npub struct Listed;\n#[derive(Serialize, schemars::JsonSchema)]\npub struct Unlisted;\n#[derive(Deserialize)]\npub struct Bare;\n",
    )?;
    repo.write(
        "crates/wire/src/stream.rs",
        "#[derive(Serialize, JsonSchema)]\npub struct Outside;\nmod inner {\n    #[derive(Serialize)]\n    pub struct Nested;\n}\n",
    )?;
    repo.track()?;
    let mut findings = Vec::new();
    contract::check(repo.root(), &mut findings)?;
    assert_eq!(findings.len(), 3, "{findings:?}");
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("events.rs:4: Unlisted is missing from ROOTS")),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("events.rs:6: Bare derives serde but not")),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("stream.rs:5: Nested derives serde but not")),
        "{findings:?}"
    );
    Ok(())
}
