mod argv;
mod auth;
pub mod capture_hook;
mod configuration;
pub mod db;
pub mod doctor;
mod error;
mod exit;
mod harness;
mod listener;
mod local;
pub mod logs;
pub mod memory;
mod open;
mod process;
mod roots;
pub mod service;
pub mod share_server;
mod status;
pub mod supervise;
pub mod tree;
pub mod update;

use config::Environment;
use tokio_util::sync::CancellationToken;

pub use error::Error;
pub use exit::Exit;
pub use local::Prepared;
pub use roots::ServiceRoot;
pub use tree::{Cli, Command, ConfigCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub stdout: String,
    pub stderr: String,
    pub exit: Exit,
    pub stdout_raw: Vec<u8>,
}

impl Report {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit: Exit::Ok,
            stdout_raw: Vec::new(),
        }
    }

    pub fn failure(error: &impl std::fmt::Display) -> Self {
        Self::error(error, Exit::Failure)
    }

    pub fn error(error: &impl std::fmt::Display, exit: Exit) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("Error: {error}\n"),
            exit,
            stdout_raw: Vec::new(),
        }
    }

    pub fn write(self) -> Result<Exit, std::io::Error> {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(self.stdout.as_bytes())?;
        stdout.write_all(&self.stdout_raw)?;
        std::io::stderr().lock().write_all(self.stderr.as_bytes())?;
        Ok(self.exit)
    }
}

#[derive(Debug)]
pub enum Dispatch {
    Gateway {
        check: bool,
        config: Option<std::path::PathBuf>,
    },
    Worker {
        role: workers::ProcessRole,
        check: bool,
        config: Option<std::path::PathBuf>,
    },
    Mcp {
        check: bool,
        config: Option<std::path::PathBuf>,
    },
    Local(Box<Prepared>),
    Complete(Report),
}

pub async fn dispatch(cli: Cli, env: &Environment, shutdown: &CancellationToken) -> Dispatch {
    let Some(command) = cli.command else {
        return Dispatch::Complete(Report::success(Cli::help()));
    };
    let structured_shutdown = matches!(
        command,
        Command::Update(_)
            | Command::Db(_)
            | Command::Service { .. }
            | Command::Start { .. }
            | Command::Local { .. }
            | Command::Logs { .. }
    );
    let operation = command_dispatch(command, env, shutdown);
    let result = if structured_shutdown {
        operation.await
    } else {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => Err(Error::Cancelled),
            result = operation => result,
        }
    };
    match result {
        Ok(dispatch) => dispatch,
        Err(error) => {
            let exit = if let Error::State(config::StateError::AlreadyRunning { .. }) = &error {
                Exit::AlreadyRunning
            } else {
                Exit::Failure
            };
            Dispatch::Complete(Report::error(&error, exit))
        }
    }
}

async fn command_dispatch(
    command: Command,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Dispatch, Error> {
    match command {
        Command::Start { config } => {
            return supervise::Supervision::production()
                .run(env, config, shutdown)
                .await
                .map(Dispatch::Complete);
        }
        Command::Local {
            port,
            no_lan,
            ensure,
            wait,
            json,
            no_cloud,
        } => {
            let request = local::Ensure {
                port,
                no_lan,
                no_cloud: no_cloud || env.cloud_switched_off()?,
                wait_seconds: wait,
            };
            if ensure {
                return Ok(Dispatch::Complete(local::render(
                    local::ensure(env, request).await,
                    json,
                )));
            }
            return run_local(env, request, shutdown).await;
        }
        Command::Gateway { check, config } => {
            return Ok(Dispatch::Gateway { check, config });
        }
        Command::Worker {
            role,
            check,
            config,
        } => {
            return Ok(Dispatch::Worker {
                role,
                check,
                config,
            });
        }
        Command::Mcp { check, config } => return Ok(Dispatch::Mcp { check, config }),
        Command::Version => Ok(Report::success(version())),
        Command::Config { command } => configuration::run(command, env).await,
        Command::Doctor { json } => doctor::run(env, json).await,
        Command::Auth { command } => auth::run(command, env).await,
        Command::Open { no_browser } => open::run(env, no_browser).await,
        Command::Status => Ok(status::run(env).await),
        Command::Stop => process::stop(env).await,
        Command::Service { command } => service::Scope::for_uid(nix::unistd::geteuid().as_raw())
            .run(command, env, shutdown)
            .await
            .map_err(Error::from),
        Command::Update(args) => update::run(args, env, shutdown).await,
        Command::Db(args) => db::run(args, env, shutdown).await,
        Command::Capture { command } => capture_hook::run(command, env).await,
        Command::Share { command } => share_server::run(command, env, shutdown).await,
        Command::Harness { command } => harness::run(command, env).await,
        Command::Memory { args } => Ok(memory::execute(
            &args,
            env,
            shutdown,
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        )
        .await),
        Command::Logs { args } => {
            logs::execute(
                service::Scope::for_uid(nix::unistd::geteuid().as_raw()),
                &args,
                env,
                shutdown,
            )
            .await
        }
    }
    .map(Dispatch::Complete)
}

async fn run_local(
    env: &Environment,
    request: local::Ensure,
    shutdown: &CancellationToken,
) -> Result<Dispatch, Error> {
    let prepared = local::prepare(env, request.port, request.no_lan, request.no_cloud).await?;
    if shutdown.is_cancelled() {
        prepared.release()?;
        return Ok(Dispatch::Complete(Report::success("")));
    }
    Ok(Dispatch::Local(Box::new(prepared)))
}

fn version() -> String {
    let version = gateway::VERSION;
    match gateway::REVISION {
        Some(revision) => format!("RivetHub v{version} ({revision})\n"),
        None => format!("RivetHub v{version}\n"),
    }
}
