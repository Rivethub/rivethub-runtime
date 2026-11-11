use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::Signal;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

use crate::process::guard::Guarded;
use crate::process::guard::SpawnGuarded;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            !stat.trim().is_empty() && !stat.trim().starts_with('Z')
        })
}

async fn gone(pid: u32) -> bool {
    for _ in 0..50 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn shell(script: &str) -> TestResult<(Guarded, u32)> {
    let mut child = Command::new("sh")
        .args(["-c", script])
        .stdout(Stdio::piped())
        .spawn_guarded()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let line = BufReader::new(stdout)
        .lines()
        .next_line()
        .await?
        .ok_or("the shell printed no grandchild pid")?;
    Ok((child, line.trim().parse()?))
}

const TERM_IGNORING: &str = "trap '' TERM; sleep 300 & echo $!; wait";

#[tokio::test]
async fn dropping_the_guard_kills_the_whole_tree() -> TestResult {
    let (child, grandchild) = shell("sleep 300 & echo $!; wait").await?;
    assert!(alive(grandchild));
    drop(child);
    assert!(gone(grandchild).await, "grandchild {grandchild} survived");
    Ok(())
}

#[tokio::test]
async fn a_term_ignoring_child_is_killed_by_pid_after_the_grace() -> TestResult {
    let (mut child, grandchild) = shell(TERM_IGNORING).await?;
    let pid = child.id().ok_or("no pid")?;
    let (status, escalated) = child
        .stop(Signal::SIGTERM, Duration::from_millis(500))
        .await?;
    assert!(escalated, "SIGTERM alone should not stop it: {status}");
    assert!(!alive(pid));
    drop(child);
    assert!(gone(grandchild).await, "grandchild {grandchild} survived");
    Ok(())
}

#[tokio::test]
async fn a_child_that_honors_sigterm_is_not_escalated() -> TestResult {
    let (mut child, grandchild) = shell("sleep 300 & echo $!; wait").await?;
    let (_, escalated) = child.stop(Signal::SIGTERM, Duration::from_secs(10)).await?;
    assert!(!escalated);
    drop(child);
    assert!(gone(grandchild).await, "grandchild {grandchild} survived");
    Ok(())
}
