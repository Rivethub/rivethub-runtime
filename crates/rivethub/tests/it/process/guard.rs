use std::io;
use std::ops::Deref;
use std::ops::DerefMut;
use std::process::ExitStatus;
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use tokio::process::Child;
use tokio::process::Command;

#[derive(Debug)]
pub struct Guarded(Child);

pub trait SpawnGuarded {
    fn spawn_guarded(&mut self) -> io::Result<Guarded>;
}

impl SpawnGuarded for Command {
    fn spawn_guarded(&mut self) -> io::Result<Guarded> {
        self.kill_on_drop(true).spawn().map(Guarded)
    }
}

impl Deref for Guarded {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for Guarded {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Guarded {
    pub async fn stop(
        &mut self,
        signal: Signal,
        grace: Duration,
    ) -> io::Result<(ExitStatus, bool)> {
        let Some(pid) = self.0.id() else {
            return self.wait().await.map(|status| (status, false));
        };
        let tree = self.tree();
        signal_pid(pid, signal);
        let (status, escalated) =
            if let Ok(status) = tokio::time::timeout(grace, self.0.wait()).await {
                (status?, false)
            } else {
                signal_pid(pid, Signal::SIGKILL);
                (self.0.wait().await?, true)
            };
        sweep(&tree);
        Ok((status, escalated))
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let tree = self.tree();
        let status = self.0.wait().await?;
        sweep(&tree);
        Ok(status)
    }

    pub async fn kill(&mut self) -> io::Result<()> {
        let tree = self.tree();
        self.0.kill().await?;
        sweep(&tree);
        Ok(())
    }

    fn tree(&self) -> Vec<u32> {
        self.0.id().map(descendants).unwrap_or_default()
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let Some(pid) = self.0.id() else {
            return;
        };
        let mut tree = descendants(pid);
        tree.push(pid);
        sweep(&tree);
    }
}

fn sweep(pids: &[u32]) {
    for pid in pids {
        signal_pid(*pid, Signal::SIGKILL);
    }
}

fn signal_pid(pid: u32, signal: Signal) {
    if let Ok(raw) = i32::try_from(pid) {
        kill(Pid::from_raw(raw), signal).ok();
    }
}

fn descendants(root: u32) -> Vec<u32> {
    let Ok(listing) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid="])
        .output()
    else {
        return Vec::new();
    };
    let edges: Vec<(u32, u32)> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut found = vec![root];
    let mut next = 0;
    while let Some(&parent) = found.get(next) {
        next += 1;
        for &(pid, ppid) in &edges {
            if ppid == parent && !found.contains(&pid) {
                found.push(pid);
            }
        }
    }
    found.remove(0);
    found.reverse();
    found
}
