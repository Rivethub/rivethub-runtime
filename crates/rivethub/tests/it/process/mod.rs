use guard::SpawnGuarded;
use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{ChildStderr, Command};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub struct Running {
    child: guard::Guarded,
    lines: Lines<BufReader<ChildStderr>>,
    pub address: SocketAddr,
}

pub fn prepare_home(home: &std::path::Path) -> TestResult {
    std::fs::create_dir_all(home.join(".rivethub"))?;
    Ok(())
}

impl Running {
    pub async fn start(home: &std::path::Path) -> TestResult<Self> {
        Self::start_with_environment(home, &[]).await
    }

    pub async fn start_with_environment(
        home: &std::path::Path,
        environment: &[(&str, &str)],
    ) -> TestResult<Self> {
        prepare_home(home)?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
            .arg("gateway")
            .env_clear()
            .env("HOME", home)
            .env("RIVETHUB_SHARED_DIR", home)
            .envs(environment.iter().copied())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn_guarded()?;
        let stderr = child.stderr.take().ok_or("missing stderr")?;
        let mut lines = BufReader::new(stderr).lines();
        let address = tokio::time::timeout(Duration::from_secs(30), async {
            let mut output = Vec::new();
            while let Some(line) = lines.next_line().await? {
                if let Some((_, address)) = line.split_once("gateway listening on ") {
                    let address = address.split_whitespace().next().ok_or("missing address")?;
                    return address.parse::<SocketAddr>().map_err(Into::into);
                }
                output.push(line);
            }
            Err::<SocketAddr, Box<dyn std::error::Error>>(
                format!("gateway stopped before readiness: {}", output.join("\n")).into(),
            )
        })
        .await??;
        Ok(Self {
            child,
            lines,
            address,
        })
    }

    pub async fn stop(mut self, signal: nix::sys::signal::Signal) -> TestResult<Vec<String>> {
        let pid = self.child.id().ok_or("gateway already stopped")?;
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(pid)?), signal)?;
        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(line) = self.lines.next_line().await? {
                output.push(line);
            }
            let status = self.child.wait().await?;
            assert!(status.success(), "gateway exit: {status}");
            Ok::<_, std::io::Error>(())
        })
        .await??;
        Ok(output)
    }
}
pub mod guard;
pub mod provider;
