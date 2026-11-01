use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;

use kernel::ca::Principal;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::Error;
use crate::roster::Roster;

const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provisioner {
    Nodes,
    Agents,
    Devices,
}

impl Provisioner {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nodes => "nodes",
            Self::Agents => "agents",
            Self::Devices => "devices",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPlan {
    pub subject: String,
    pub sans: Vec<String>,
    pub provisioner: Provisioner,
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: Url,
    pub root: PathBuf,
    pub password_file: PathBuf,
}

#[derive(Clone, Debug)]
pub enum CliToken {
    Node(String),
    Agent(String),
}

pub struct ProcessEnv {
    pairs: Vec<(OsString, OsString)>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "B14 process environment values include provisioner material; derived Debug would print them"
    )
)]
impl std::fmt::Debug for ProcessEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessEnv")
            .field(
                "keys",
                &self.pairs.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ProcessEnv {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            pairs: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.pairs
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }
}

pub struct Minted {
    pub token: String,
    pub stderr: String,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "B14 one-time enrollment tokens are secrets; derived Debug would print them"
    )
)]
impl std::fmt::Debug for Minted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Minted")
            .field("token", &"[redacted]")
            .field("stderr", &self.stderr)
            .finish()
    }
}

struct Captured {
    stdout: String,
    stderr: String,
}

pub fn plan_cli(kind: CliToken, roster: Option<&Roster>) -> Result<TokenPlan, Error> {
    match kind {
        CliToken::Node(name) => node_plan(&name, roster),
        CliToken::Agent(name) => agent_plan(&name, roster),
    }
}

pub fn plan_principal(principal: &Principal, roster: Option<&Roster>) -> Result<TokenPlan, Error> {
    match principal {
        Principal::Node { node } => node_plan(node, roster),
        Principal::Agent { agent, .. } => agent_plan(agent, roster),
        Principal::Device { .. } => {
            let uri = principal.spiffe_uri().map_err(|_| Error::DevicePrincipal)?;
            Ok(TokenPlan {
                subject: uri.clone(),
                sans: vec![uri],
                provisioner: Provisioner::Devices,
            })
        }
    }
}

pub async fn mint(
    plan: &TokenPlan,
    endpoint: &Endpoint,
    env: &ProcessEnv,
    cancel: &CancellationToken,
) -> Result<Minted, Error> {
    let captured = spawn(token_args(plan, endpoint), "token", env, cancel).await?;
    Ok(Minted {
        token: one_time_token(&captured.stdout)?.to_owned(),
        stderr: captured.stderr,
    })
}

pub async fn revoke_serial(
    serial: &str,
    endpoint: &Endpoint,
    env: &ProcessEnv,
    cancel: &CancellationToken,
) -> Result<(), Error> {
    canonical_serial(serial)?;
    drop(spawn(revoke_args(serial, endpoint), "revoke", env, cancel).await?);
    Ok(())
}

fn roster_of(roster: Option<&Roster>) -> Result<&Roster, Error> {
    roster.ok_or(Error::RosterRequired)
}

fn node_plan(name: &str, roster: Option<&Roster>) -> Result<TokenPlan, Error> {
    let node = roster_of(roster)?.node(name)?;
    let mesh = format!("{}.rivet.mesh", node.name);
    Ok(TokenPlan {
        subject: node.name.clone(),
        sans: vec![node.address.to_string(), node.name.clone(), mesh],
        provisioner: Provisioner::Nodes,
    })
}

fn agent_plan(name: &str, roster: Option<&Roster>) -> Result<TokenPlan, Error> {
    let (node, agent) = roster_of(roster)?.agent(name)?;
    let uri = Principal::Agent {
        node: node.name.clone(),
        agent: agent.to_owned(),
    }
    .spiffe_uri()
    .map_err(|_| Error::UnknownAgent(name.to_owned()))?;
    Ok(TokenPlan {
        subject: uri.clone(),
        sans: vec![uri],
        provisioner: Provisioner::Agents,
    })
}

fn token_args(plan: &TokenPlan, endpoint: &Endpoint) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("ca"),
        OsString::from("token"),
        OsString::from(&plan.subject),
        OsString::from("--provisioner"),
        OsString::from(plan.provisioner.as_str()),
    ];
    for san in &plan.sans {
        args.push(OsString::from("--san"));
        args.push(OsString::from(san));
    }
    push_endpoint(&mut args, endpoint);
    args
}

fn revoke_args(serial: &str, endpoint: &Endpoint) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("ca"),
        OsString::from("revoke"),
        OsString::from(serial),
    ];
    push_endpoint(&mut args, endpoint);
    args
}

fn push_endpoint(args: &mut Vec<OsString>, endpoint: &Endpoint) {
    args.push(OsString::from("--ca-url"));
    args.push(OsString::from(endpoint.url.as_str()));
    args.push(OsString::from("--root"));
    args.push(endpoint.root.as_os_str().to_owned());
    args.push(OsString::from("--provisioner-password-file"));
    args.push(endpoint.password_file.as_os_str().to_owned());
}

fn canonical_serial(serial: &str) -> Result<(), Error> {
    let mut bytes = serial.bytes();
    match bytes.next() {
        Some(first)
            if (b'1'..=b'9').contains(&first) && bytes.all(|byte| byte.is_ascii_digit()) =>
        {
            Ok(())
        }
        _ => Err(Error::Serial),
    }
}

fn one_time_token(stdout: &str) -> Result<&str, Error> {
    let token = stdout.trim_end_matches(['\r', '\n']);
    if token.is_empty() || token.bytes().any(|byte| byte.is_ascii_whitespace()) {
        Err(Error::StepToken)
    } else {
        Ok(token)
    }
}

fn command(env: &ProcessEnv) -> Command {
    let mut command = Command::new("step");
    command.env_clear().envs(env.iter()).kill_on_drop(true);
    command
}

async fn spawn(
    args: Vec<OsString>,
    action: &'static str,
    env: &ProcessEnv,
    cancel: &CancellationToken,
) -> Result<Captured, Error> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut child = command(env)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or(Error::StepPipe)?;
    let stderr = child.stderr.take().ok_or(Error::StepPipe)?;
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));
    tokio::select! {
        biased;
        () = cancel.cancelled() => {
            let killed = child.kill().await;
            stdout_task.abort();
            stderr_task.abort();
            killed?;
            Err(Error::Cancelled)
        }
        status = child.wait() => {
            let status = status?;
            let stdout = text(stdout_task.await??)?;
            let stderr = text(stderr_task.await??)?;
            if status.success() {
                Ok(Captured { stdout, stderr })
            } else {
                Err(Error::StepFailed { action, detail: stderr })
            }
        }
    }
}

async fn read_pipe(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut chunk).await?;
        if count == 0 {
            return Ok(bytes);
        }
        let next = bytes.len().saturating_add(count);
        if next > OUTPUT_LIMIT {
            return Err(Error::StepOutput);
        }
        bytes.extend(chunk.iter().take(count).copied());
    }
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::StepEncoding)
}
