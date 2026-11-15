mod checks;
mod gateway;
mod mcp;
mod start;
mod worker;

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use crate::process::{DEAD_POSTGRES, TestResult, provider_config, terminate, write_state};

async fn run_rh(home: &Path, args: &[&str]) -> TestResult<std::process::Output> {
    Ok(tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(args)
            .env_clear()
            .env("HOME", home)
            .output(),
    )
    .await??)
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn interrupt(pid: u32) -> TestResult {
    crate::process::signal(pid, nix::sys::signal::Signal::SIGINT)
}
