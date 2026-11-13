pub(crate) mod command;
mod unit;

use std::path::{Path, PathBuf};

use clap::Subcommand;
use config::{Environment, Paths};
use tokio_util::sync::CancellationToken;

use crate::{Exit, Report};
pub use unit::Unit;

#[derive(Debug, Subcommand, Clone, Copy, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum ServiceCommand {
    Init,
    Start,
    Stop,
    Restart,
    Status,
    Logs,
}

pub const UNIT_NAME: &str = "rivethub";

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum NodeKind {
    #[strum(serialize = "start")]
    Node,
    #[strum(serialize = "local")]
    Standalone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    System,
    User,
}

impl Scope {
    pub fn for_uid(uid: u32) -> Self {
        if uid == 0 { Self::System } else { Self::User }
    }

    pub fn unit_path(self, home: &Path) -> PathBuf {
        match self {
            Self::System => PathBuf::from(format!("/etc/systemd/system/{UNIT_NAME}.service")),
            Self::User => home.join(format!(".config/systemd/user/{UNIT_NAME}.service")),
        }
    }

    pub async fn read_journal(
        self,
        arguments: &[String],
        follow: bool,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<Report, ServiceError> {
        let mut command = self.command("journalctl", env);
        command.arg("-u").arg(UNIT_NAME);
        command.args(arguments);
        if follow {
            command::follow(command, shutdown).await
        } else {
            command::capture(command, "journalctl", shutdown).await
        }
    }

    fn command(self, program: &'static str, env: &Environment) -> tokio::process::Command {
        let mut command = command::command(program, env);
        if self == Self::User {
            command.arg("--user");
        }
        command
    }

    pub async fn run(
        self,
        action: ServiceCommand,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<Report, ServiceError> {
        if shutdown.is_cancelled() && !matches!(action, ServiceCommand::Logs) {
            return Err(ServiceError::Cancelled);
        }
        match action {
            ServiceCommand::Init => self.init(env, shutdown).await,
            ServiceCommand::Logs => {
                self.read_journal(
                    &["-f".to_string(), "--no-pager".to_string()],
                    true,
                    env,
                    shutdown,
                )
                .await
            }
            ServiceCommand::Start | ServiceCommand::Restart => {
                self.systemctl(&["daemon-reload"], env, shutdown)
                    .await
                    .and_then(command::require_success)?;
                self.systemctl(&[&action.to_string(), "rivethub"], env, shutdown)
                    .await
                    .and_then(command::require_success)?;
                self.systemctl(&["status", "rivethub", "--no-pager"], env, shutdown)
                    .await
            }
            ServiceCommand::Stop => self
                .systemctl(&["stop", "rivethub"], env, shutdown)
                .await
                .and_then(command::require_success),
            ServiceCommand::Status => {
                self.systemctl(&["status", "rivethub", "--no-pager"], env, shutdown)
                    .await
            }
        }
    }

    async fn systemctl(
        self,
        args: &[&str],
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<Report, ServiceError> {
        let mut command = self.command("systemctl", env);
        command.args(args);
        command::capture(command, "systemctl", shutdown).await
    }

    async fn init(
        self,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<Report, ServiceError> {
        let paths = Paths::new(env).map_err(|_| ServiceError::Home)?;
        let home = paths.home().to_path_buf();
        let executable = tokio::task::spawn_blocking(std::env::current_exe).await??;
        let owned = env.clone();
        let kind =
            tokio::task::spawn_blocking(move || crate::configuration::node_kind(&owned)).await??;
        let unit = Unit::new(&executable, &paths.env_file(), self, kind)?;
        let path = unit.install(home).await?;
        let mut message = format!(
            "Created service file: {}\nIt runs: rh {kind}\n",
            path.display()
        );
        self.systemctl(&["daemon-reload"], env, shutdown)
            .await
            .and_then(command::require_success)?;
        self.systemctl(&["enable", UNIT_NAME], env, shutdown)
            .await
            .and_then(command::require_success)?;
        message.push_str(match self {
            Self::System => "Enabled: it starts at boot.\n",
            Self::User => "Enabled: it starts when you log in, and at boot when lingering is on.\n",
        });
        if self == Self::User {
            let user =
                tokio::task::spawn_blocking(|| nix::unistd::User::from_uid(nix::unistd::geteuid()))
                    .await??
                    .ok_or(ServiceError::User)?;
            let mut command = command::command("loginctl", env);
            command.args(["show-user", &user.name, "--property=Linger", "--value"]);
            let report =
                command::require_success(command::capture(command, "loginctl", shutdown).await?)?;
            match report.stdout.trim() {
                "yes" => message.push_str("Lingering is enabled.\n"),
                "no" => {
                    message.push_str("Lingering is disabled.\nRun: sudo loginctl enable-linger ");
                    message.push_str(&user.name);
                    message.push('\n');
                }
                _ => return Err(ServiceError::Linger),
            }
        }
        Ok(Report::success(message))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("child process id is outside the platform range: {0}")]
    Pid(#[from] std::num::TryFromIntError),
    #[error("Service file already exists: {}\nDelete it first if you want to regenerate.", .0.display())]
    Exists(PathBuf),
    #[error("invalid service path: {}", .0.display())]
    Path(PathBuf),
    #[error("cannot tell which node the service should run from the config: {0}")]
    Config(#[from] config::ConfigError),
    #[error("HOME is required for service initialization")]
    Home,
    #[error("current user has no account entry")]
    User,
    #[error("loginctl returned an invalid Linger value")]
    Linger,
    #[error("{program}: {source}")]
    Process {
        program: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system command failed ({exit:?}): {stderr}")]
    Failed { exit: Exit, stderr: String },
    #[error("system command terminated: {0}")]
    Exit(std::process::ExitStatus),
    #[error("system command output pipe is unavailable")]
    Pipe,
    #[error("system command output is not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Account(#[from] nix::errno::Errno),
    #[error("service command cancelled")]
    Cancelled,
}
