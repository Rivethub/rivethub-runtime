use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use xtask::check;
use xtask::error::{Error, Result};

enum Mode {
    Check {
        root: PathBuf,
        locked: bool,
        list_tests: bool,
    },
    Baseline,
    PreCommit,
    CommitMsg {
        file: PathBuf,
    },
    Commits {
        range: String,
    },
    InstallHooks,
}

fn parse(args: &[String]) -> Result<Mode> {
    let head = args.first().map(String::as_str);
    match head {
        Some("check") => {
            let mut root = PathBuf::from(".");
            let mut locked = false;
            let mut list_tests = true;
            for arg in args.iter().skip(1) {
                if arg == "--locked" {
                    locked = true;
                } else if arg == "--no-nextest-list" {
                    list_tests = false;
                } else {
                    root = PathBuf::from(arg);
                }
            }
            Ok(Mode::Check {
                root: check::absolute(&root)?,
                locked,
                list_tests,
            })
        }
        Some("baseline") if args.len() == 2 && args.get(1).is_some_and(|arg| arg == "--write") => Ok(Mode::Baseline),
        Some("pre-commit") => Ok(Mode::PreCommit),
        Some("commit-msg") => {
            let file = args
                .get(1)
                .ok_or_else(|| Error::Usage("commit-msg FILE".to_owned()))?;
            Ok(Mode::CommitMsg {
                file: PathBuf::from(file),
            })
        }
        Some("commits") => {
            let range = args
                .get(1)
                .ok_or_else(|| Error::Usage("commits RANGE".to_owned()))?;
            Ok(Mode::Commits {
                range: range.clone(),
            })
        }
        Some("install-hooks") => Ok(Mode::InstallHooks),
        _ => Err(Error::Usage(
            "usage: cargo xtask check [--locked] [--no-nextest-list]|baseline --write|pre-commit|commit-msg FILE|commits RANGE|install-hooks"
                .to_owned(),
        )),
    }
}

fn hook_mode(argv0: Option<&str>, args: &[String]) -> Option<Result<Mode>> {
    let invoked = argv0
        .and_then(|name| Path::new(name).file_name())?
        .to_str()?;
    match invoked {
        "pre-commit" => Some(Ok(Mode::PreCommit)),
        "commit-msg" => Some(match args.first() {
            Some(file) => Ok(Mode::CommitMsg {
                file: PathBuf::from(file),
            }),
            None => Err(Error::Usage("commit-msg FILE".to_owned())),
        }),
        _ => None,
    }
}

fn main() -> ExitCode {
    let mut raw = std::env::args();
    let invoked_as = raw.next();
    let args: Vec<String> = raw.collect();
    let root = match check::absolute(&PathBuf::from(".")) {
        Ok(root) => root,
        Err(error) => return best_effort_report(&error),
    };
    let outcome = match hook_mode(invoked_as.as_deref(), &args) {
        Some(mode) => mode,
        None => parse(&args),
    }
    .and_then(|mode| match mode {
        Mode::Check {
            root,
            locked,
            list_tests,
        } => check::check(&root, locked, list_tests),
        Mode::Baseline => xtask::baseline::write(&root),
        Mode::PreCommit => check::pre_commit(&root),
        Mode::CommitMsg { file } => check::commit_msg(&root, &file),
        Mode::Commits { range } => check::commits(&root, &range),
        Mode::InstallHooks => check::install_hooks(&root),
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Error::Finding(_)) => ExitCode::FAILURE,
        Err(error) => best_effort_report(&error),
    }
}

#[expect(clippy::let_underscore_must_use)]
#[expect(clippy::let_underscore_untyped)]
fn best_effort_report(error: &Error) -> ExitCode {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    let _ = writeln!(out, "xtask: {error}");
    let _ = out.flush();
    ExitCode::FAILURE
}
