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

/// A spawned child that takes its whole process tree with it when the test ends.
///
/// `kill_on_drop` alone signals only the direct child, so a failed or timed-out
/// test left the `rh gateway` or `rh local` grandchildren of `rh start` running
/// with their database connections open. The tree is found through `ps` and each
/// member gets SIGKILL on drop. The child stays in the test's own process group,
/// so a group-wide signal from nextest or a terminal still reaches it too.
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
    /// Sends `signal` to the child, waits up to `grace` for it to exit, and on
    /// timeout sends SIGKILL to that exact pid. Returns the exit status and
    /// whether the escalation was needed. Descendants that outlive it are killed.
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

    /// Like `Child::wait`, then kills any descendant the child left running.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let tree = self.tree();
        let status = self.0.wait().await?;
        sweep(&tree);
        Ok(status)
    }

    /// Like `Child::kill`, then kills any descendant the child left running.
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

/// Every transitive child of `root`, deepest first.
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
