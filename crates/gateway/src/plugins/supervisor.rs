use std::os::unix::process::ExitStatusExt as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::io::{AsyncRead, BufReader};
use tokio::process::Child;
use tokio_util::sync::CancellationToken;
use wire::plugin::PluginId;
use wire::plugin::host::{PluginExit, PluginLogStream};

use super::{PluginHost, launch};

const READY_POLL: Duration = Duration::from_millis(100);
const HEALTHY_RUN: Duration = Duration::from_secs(600);

enum Ended {
    Stopped,
    Failed {
        exit: Option<PluginExit>,
        reason: String,
    },
}

enum Readiness {
    Ready,
    Stopped,
    Exited(PluginExit),
    Late,
}

pub(super) async fn supervise(host: Arc<PluginHost>, plugin: PluginId, stop: CancellationToken) {
    let mut failures = 0_u32;
    loop {
        let began = Instant::now();
        match run_once(&host, &plugin, &stop).await {
            Ended::Stopped => break,
            Ended::Failed { exit, reason } => {
                failures = if began.elapsed() >= HEALTHY_RUN {
                    1
                } else {
                    failures + 1
                };
                let delay = kernel::plugins::restart_delay(failures);
                host.backoff(&plugin, exit, &reason, delay);
                tokio::select! {
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
    host.supervisor_stopped(&plugin);
}

async fn run_once(host: &Arc<PluginHost>, plugin: &PluginId, stop: &CancellationToken) -> Ended {
    let prepared = match host.prepare(plugin).await {
        Ok(prepared) => prepared,
        Err(reason) => return Ended::Failed { exit: None, reason },
    };
    let launch::Launched {
        mut child,
        confinement,
    } = match launch::spawn(&prepared.plan).await {
        Ok(launched) => launched,
        Err(error) => {
            return Ended::Failed {
                exit: None,
                reason: format!(
                    "could not start {}: {error}",
                    prepared.plan.program.display()
                ),
            };
        }
    };
    host.log(
        plugin,
        PluginLogStream::Host,
        format!(
            "started {}, {}",
            prepared.plan.program.display(),
            confinement.describe()
        ),
    );
    pump(host, plugin, child.stdout.take(), PluginLogStream::Stdout);
    pump(host, plugin, child.stderr.take(), PluginLogStream::Stderr);
    let client = match reqwest::Client::builder()
        .unix_socket(prepared.socket.clone())
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            stop_child(host, plugin, &mut child).await;
            return Ended::Failed {
                exit: None,
                reason: format!("socket client: {error}"),
            };
        }
    };
    match ready(&client, &mut child, prepared.ready_within, stop).await {
        Readiness::Ready => {}
        Readiness::Stopped => {
            stop_child(host, plugin, &mut child).await;
            return Ended::Stopped;
        }
        Readiness::Exited(exit) => {
            return Ended::Failed {
                exit: Some(exit),
                reason: "exited before it was ready".to_owned(),
            };
        }
        Readiness::Late => {
            let exit = stop_child(host, plugin, &mut child).await;
            return Ended::Failed {
                exit,
                reason: format!("not ready within {} ms", prepared.ready_within.as_millis()),
            };
        }
    }
    host.running(plugin, client);
    tokio::select! {
        status = child.wait() => Ended::Failed {
            exit: Some(exit_of(status)),
            reason: "exited".to_owned(),
        },
        () = stop.cancelled() => {
            stop_child(host, plugin, &mut child).await;
            Ended::Stopped
        }
    }
}

async fn ready(
    client: &reqwest::Client,
    child: &mut Child,
    within: Duration,
    stop: &CancellationToken,
) -> Readiness {
    let deadline = Instant::now() + within;
    loop {
        tokio::select! {
            () = stop.cancelled() => return Readiness::Stopped,
            status = child.wait() => return Readiness::Exited(exit_of(status)),
            () = tokio::time::sleep(READY_POLL) => {}
        }
        if healthy(client).await {
            return Readiness::Ready;
        }
        if Instant::now() >= deadline {
            return Readiness::Late;
        }
    }
}

async fn healthy(client: &reqwest::Client) -> bool {
    match client
        .get(super::plugin_url("health"))
        .timeout(READY_POLL * 10)
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(error) => {
            tracing::trace!(%error, "plug-in not answering its health route yet");
            false
        }
    }
}

async fn stop_child(host: &PluginHost, plugin: &PluginId, child: &mut Child) -> Option<PluginExit> {
    match launch::terminate(child).await {
        Ok(status) => {
            let exit = exit_of(Ok(status));
            host.log(
                plugin,
                PluginLogStream::Host,
                format!("stopped ({})", describe(&exit)),
            );
            Some(exit)
        }
        Err(error) => {
            host.log(
                plugin,
                PluginLogStream::Host,
                format!("stop failed: {error}"),
            );
            None
        }
    }
}

fn exit_of(status: std::io::Result<std::process::ExitStatus>) -> PluginExit {
    match status {
        Ok(status) => PluginExit {
            at: Utc::now(),
            code: status.code(),
            signal: status.signal().map(signal_name),
        },
        Err(error) => {
            tracing::warn!(%error, "plug-in exit status unreadable");
            PluginExit {
                at: Utc::now(),
                code: None,
                signal: None,
            }
        }
    }
}

fn signal_name(raw: i32) -> String {
    match nix::sys::signal::Signal::try_from(raw) {
        Ok(signal) => signal.as_str().to_owned(),
        Err(error) => format!("signal {raw} ({error})"),
    }
}

pub(super) fn describe(exit: &PluginExit) -> String {
    match (&exit.code, &exit.signal) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(signal)) => format!("killed by {signal}"),
        (None, None) => "exit status unknown".to_owned(),
    }
}

fn pump<R>(host: &Arc<PluginHost>, plugin: &PluginId, output: Option<R>, stream: PluginLogStream)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let Some(output) = output else {
        return;
    };
    let host = Arc::clone(host);
    let plugin = plugin.clone();
    host.tasks.clone().spawn(async move {
        let mut reader = BufReader::new(output);
        loop {
            match super::output::next_line(&mut reader, super::output::MAX_LINE_BYTES).await {
                Ok(Some(line)) => host.log(&plugin, stream, line),
                Ok(None) => break,
                Err(error) => {
                    host.log(
                        &plugin,
                        PluginLogStream::Host,
                        format!("output unreadable: {error}"),
                    );
                    break;
                }
            }
        }
    });
}
