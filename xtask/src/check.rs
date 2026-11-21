use crate::error::{Error, Result};
use crate::git::git;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn finish(label: &str, findings: &[String]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for finding in findings {
        writeln!(out, "{finding}")?;
    }
    let state = if findings.is_empty() { "OK" } else { "FAIL" };
    writeln!(out, "GATE {state}: {} finding(s), {label}", findings.len())?;
    out.flush()?;
    if findings.is_empty() {
        Ok(())
    } else {
        Err(Error::Finding(format!("{} finding(s)", findings.len())))
    }
}

fn offline_cargo(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(args)
        .env("SQLX_OFFLINE", "true");
    command
}

pub fn run_tool(root: &Path, args: &[&str]) -> Result<()> {
    let status = offline_cargo(root, args).status().map_err(Error::Io)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Command {
            command: format!("cargo {}", args.join(" ")),
            stderr: String::new(),
        })
    }
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

pub fn machete(root: &Path) -> Result<()> {
    machete_with(root, "cargo-machete")
}

fn machete_with(root: &Path, program: &str) -> Result<()> {
    match Command::new(program).current_dir(root).status() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Error::MissingTool {
            tool: program.to_owned(),
            install: "cargo install cargo-machete --locked".to_owned(),
        }),
        Err(error) => Err(Error::Io(error)),
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(Error::Command {
            command: program.to_owned(),
            stderr: String::new(),
        }),
    }
}

pub fn sqlx_prepare_check(root: &Path) -> Result<()> {
    if !database_url_set() {
        return Ok(());
    }
    sqlx_prepare_check_with(root, "cargo-sqlx")
}

fn nextest_list_with(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .current_dir(root)
        .args(args)
        .env("SQLX_OFFLINE", "true")
        .status()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    tool: "cargo-nextest".to_owned(),
                    install: "cargo install cargo-nextest --locked".to_owned(),
                }
            } else {
                Error::Io(error)
            }
        })?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Command {
        command: format!("cargo {}", args.join(" ")),
        stderr: String::new(),
    })
}

fn nextest_list(root: &Path, locked: bool) -> Result<()> {
    let mut args = vec!["nextest", "list", "--workspace", "--all-features"];
    if locked {
        args.push("--locked");
    }
    nextest_list_with(root, "cargo-nextest", &args)
}

fn database_url_set() -> bool {
    std::env::vars_os().any(|(name, _)| name == OsStr::new("DATABASE_URL"))
}

fn sqlx_prepare_check_with(root: &Path, program: &str) -> Result<()> {
    let status = Command::new(program)
        .current_dir(root)
        .args(["sqlx", "prepare", "--workspace", "--check"])
        .env("SQLX_OFFLINE", "false")
        .status()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::MissingTool {
                    tool: "sqlx-cli".to_owned(),
                    install: "cargo install sqlx-cli --locked --no-default-features --features rustls,postgres"
                        .to_owned(),
                }
            } else {
                Error::Io(error)
            }
        })?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Command {
        command: "cargo sqlx prepare --workspace --check".to_owned(),
        stderr: String::new(),
    })
}

pub fn check(root: &Path, locked: bool, list_tests: bool) -> Result<()> {
    run_tool(root, &["fmt", "--all", "--check"])?;
    let mut clippy = vec!["clippy"];
    if locked {
        clippy.push("--locked");
    }
    clippy.extend([
        "--workspace",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ]);
    run_tool(root, &clippy)?;
    if list_tests {
        nextest_list(root, locked)?;
    }
    let fallback_hits = crate::fallback::collect(root)?;
    let mut deny = vec!["deny", "--all-features"];
    if locked {
        deny.push("--locked");
    }
    deny.push("check");
    run_tool(root, &deny)?;
    machete(root)?;
    sqlx_prepare_check(root)?;

    let mut findings = Vec::new();
    crate::lang::check(root, &mut findings)?;
    crate::code::check_code(root, &mut findings)?;
    crate::manifests::manifests(root, &mut findings)?;
    crate::manifests::lint_policy(root, &mut findings)?;
    crate::toolconfig::check(root, &mut findings)?;
    crate::dupfixture::check(root, &mut findings)?;
    crate::labpath::check(root, &mut findings)?;
    crate::contract::check(root, &mut findings)?;
    crate::lang::privacy_and_secrets(root, &mut findings)?;
    let baseline = crate::baseline::Baseline::load(root)?;
    crate::baseline::validate(&baseline, &mut findings);
    let ast_hits = crate::rustsrc::collect(root)?;
    for check in [
        "ENV",
        "PRIVILEGE",
        "PATHCONTAIN",
        "WAIVER",
        "UTF16",
        "JSMODEL",
        "JSONBYTES",
        "PORTS",
    ] {
        let hits = ast_hits
            .iter()
            .filter(|(name, _)| *name == check)
            .map(|(_, hit)| hit.clone())
            .collect::<Vec<_>>();
        crate::baseline::adjudicate(&baseline, check, &hits, &mut findings);
    }
    crate::baseline::adjudicate(&baseline, "FALLBACK", &fallback_hits, &mut findings);
    crate::baseline::shrink(root, &mut findings)?;
    finish("cargo xtask check", &findings)
}

pub fn pre_commit(root: &Path) -> Result<()> {
    let mut findings = Vec::new();
    crate::lang::staged_privacy(root, &mut findings)?;
    let status = Command::new("cargo")
        .current_dir(root)
        .args(["fmt", "--all", "--check"])
        .status()
        .map_err(Error::Io)?;
    if !status.success() {
        findings.push("FMT staged changes must be formatted (cargo fmt --all)".to_owned());
    }
    finish("pre-commit", &findings)
}

pub fn commit_msg(root: &Path, file: &Path) -> Result<()> {
    let ident = git(root, &["var", "GIT_AUTHOR_IDENT"])?;
    let author = ident
        .split_once(" <")
        .and_then(|(name, rest)| Some((name, rest.split_once('>')?.0)));
    let text = std::fs::read_to_string(file)?;
    let mut findings = Vec::new();
    crate::authorship::commit_message(&mut findings, &text, author);
    finish("commit-msg", &findings)
}

pub fn commits(root: &Path, range: &str) -> Result<()> {
    let mut findings = Vec::new();
    crate::authorship::commits(root, range, &mut findings)?;
    finish(&format!("commits {range}"), &findings)
}

pub fn install_hooks(root: &Path) -> Result<()> {
    let binary = std::env::current_exe()?;
    install_hooks_from(root, &binary)
}

pub fn install_hooks_from(root: &Path, binary: &Path) -> Result<()> {
    copy_hooks(root, binary)?;
    git(root, &["config", "core.hooksPath", "target/git-hooks"])?;
    Ok(())
}

pub fn copy_hooks(root: &Path, binary: &Path) -> Result<()> {
    let hooks = root.join("target").join("git-hooks");
    std::fs::create_dir_all(&hooks)?;
    for name in ["pre-commit", "commit-msg"] {
        let hook = hooks.join(name);
        std::fs::copy(binary, &hook)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&hook)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&hook, permissions)?;
        }
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "hooks installed: target/git-hooks/pre-commit, target/git-hooks/commit-msg (core.hooksPath = target/git-hooks)"
    )?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::machete_with;
    use super::nextest_list_with;
    use super::sqlx_prepare_check_with;
    use std::path::Path;

    #[test]
    fn missing_machete_names_the_install_command() {
        let error = machete_with(Path::new("."), "xtask-machete-missing").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cargo install cargo-machete --locked"),
            "the error must carry the install command, got: {error}"
        );
    }

    #[test]
    fn machete_failure_is_a_command_error() {
        let error = machete_with(Path::new("."), "false").unwrap_err();
        assert!(
            matches!(error, crate::error::Error::Command { ref command, .. } if command == "false"),
            "a non-zero exit must fail the gate as a command error, got: {error}"
        );
    }

    #[test]
    fn missing_sqlx_names_the_install_command() {
        let error = sqlx_prepare_check_with(Path::new("."), "xtask-sqlx-missing").unwrap_err();
        assert!(
            error.to_string().contains(
                "cargo install sqlx-cli --locked --no-default-features --features rustls,postgres"
            ),
            "the error must carry the install command, got: {error}"
        );
    }

    #[test]
    fn sqlx_prepare_failure_is_a_command_error() {
        let error = sqlx_prepare_check_with(Path::new("."), "false").unwrap_err();
        assert!(
            matches!(error, crate::error::Error::Command { ref command, .. }
                if command == "cargo sqlx prepare --workspace --check"),
            "a non-zero exit must fail the gate as a command error, got: {error}"
        );
    }

    #[test]
    fn missing_nextest_names_the_install_command() {
        let error = nextest_list_with(
            Path::new("."),
            "xtask-nextest-missing",
            &["nextest", "list"],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cargo install cargo-nextest --locked"),
            "the error must carry the install command, got: {error}"
        );
    }

    #[test]
    fn nextest_list_failure_is_a_command_error() {
        let error = nextest_list_with(Path::new("."), "false", &["nextest", "xtask-nextest-fails"])
            .unwrap_err();
        assert!(
            matches!(error, crate::error::Error::Command { ref command, .. }
                if command == "cargo nextest xtask-nextest-fails"),
            "a test target that does not speak the list protocol must fail the gate, got: {error}"
        );
    }
}
