pub(crate) mod command;
mod unit;

use std::path::{Path, PathBuf};

use clap::Subcommand;
use config::{Environment, Paths};
use tokio_util::sync::CancellationToken;

use crate::{Exit, Report};
pub use unit::Unit;
pub(crate) use unit::ignored_environment_files;

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
                let repair = self.repair_preface(env).await?;
                let preface = self.take_over_detached(env, shutdown).await?;
                self.systemctl(&["daemon-reload"], env, shutdown)
                    .await
                    .and_then(command::require_success)?;
                self.systemctl(&[&action.to_string(), "rivethub"], env, shutdown)
                    .await
                    .and_then(command::require_success)?;
                let mut report = self
                    .systemctl(&["status", "rivethub", "--no-pager"], env, shutdown)
                    .await?;
                report.stdout.insert_str(0, &preface);
                report.stdout.insert_str(0, &repair);
                Ok(report)
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
        let placement = tokio::task::spawn_blocking(move || unit.place(&home)).await??;
        let mut message = match placement {
            unit::Placement::Created(path) => format!(
                "Created service file: {}\nIt runs: rh {kind}\n",
                path.display()
            ),
            unit::Placement::Current(path) => format!(
                "Service file is already current: {}\nIt runs: rh {kind}\n",
                path.display()
            ),
            unit::Placement::Updated { note } => format!("{note}It runs: rh {kind}\n"),
            unit::Placement::Foreign { message } => return Err(ServiceError::Edited(message)),
        };
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

    pub(crate) async fn repair_preface(self, env: &Environment) -> Result<String, ServiceError> {
        let env = env.clone();
        tokio::task::spawn_blocking(move || repair_unit(self, &env)).await?
    }

    pub(crate) async fn reload(
        self,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<(), ServiceError> {
        self.systemctl(&["daemon-reload"], env, shutdown)
            .await
            .and_then(command::require_success)
            .map(|_| ())
    }

    async fn take_over_detached(
        self,
        env: &Environment,
        shutdown: &CancellationToken,
    ) -> Result<String, ServiceError> {
        let Some(pid) = detached_pid(env).await? else {
            return Ok(String::new());
        };
        let main = self
            .systemctl(
                &["show", "-p", "MainPID", "--value", UNIT_NAME],
                env,
                shutdown,
            )
            .await
            .and_then(command::require_success)?;
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "PATTERNS FALLBACK: a MainPID that is not a number is not the detached node, so the service still stops that node"
            )
        )]
        if main.stdout.trim().parse::<u32>().ok() == Some(pid) {
            return Ok(String::new());
        }
        if !stop_detached(env, pid).await? {
            return Ok(String::new());
        }
        Ok(format!(
            "Stopped the running RivetHub node (pid {pid}) so the service can take its place.\n"
        ))
    }
}

async fn detached_pid(env: &Environment) -> Result<Option<u32>, ServiceError> {
    let pid = match crate::process::read_pid(env).await {
        Ok(Some(pid)) => pid,
        Ok(None) | Err(crate::Error::Pid) => return Ok(None),
        Err(error) => return Err(process_error(error)),
    };
    match nix::sys::signal::kill(pid, None) {
        Ok(()) | Err(nix::errno::Errno::EPERM) => {}
        Err(nix::errno::Errno::ESRCH) => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(u32::try_from(pid.as_raw())?))
}

async fn stop_detached(env: &Environment, pid: u32) -> Result<bool, ServiceError> {
    let refused = ServiceError::Running { pid };
    let report = crate::process::stop(env).await.map_err(|_| refused)?;
    if !report.stdout.starts_with("Sent SIGTERM") {
        return Ok(false);
    }
    let raw = i32::try_from(pid)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match process_gone(raw) {
            Ok(true) => return Ok(true),
            Ok(false) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ServiceError::Running { pid });
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn process_gone(pid: i32) -> Result<bool, nix::errno::Errno> {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Err(nix::errno::Errno::ESRCH) => Ok(true),
        Err(nix::errno::Errno::EPERM) => Ok(false),
        Err(error) => Err(error),
        Ok(()) => Ok(zombie(pid)),
    }
}

fn zombie(pid: i32) -> bool {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a missing /proc stat is not a zombie, so the service keeps waiting until the process is gone"
        )
    )]
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    text.rsplit_once(')')
        .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
}

fn repair_unit(scope: Scope, env: &Environment) -> Result<String, ServiceError> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a service start that cannot resolve its home leaves the unit unchanged and still starts"
        )
    )]
    let Ok(paths) = Paths::new(env) else {
        return Ok(String::new());
    };
    let home = paths.home().to_path_buf();
    let path = scope.unit_path(&home);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Ok(format!(
                "The service file {} is not a regular file, so it was left unchanged.\n",
                path.display()
            ));
        }
        Ok(_) => {}
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a service start that cannot read its config leaves the unit unchanged and still starts"
        )
    )]
    let Ok(kind) = crate::configuration::node_kind(env) else {
        return Ok(String::new());
    };
    let executable = std::env::current_exe()?;
    let unit = Unit::new(&executable, &paths.env_file(), scope, kind)?;
    match unit.place(&home)? {
        unit::Placement::Created(_) | unit::Placement::Current(_) => Ok(String::new()),
        unit::Placement::Updated { note } => Ok(note),
        unit::Placement::Foreign { message } => Ok(finish_line(message)),
    }
}

fn finish_line(message: String) -> String {
    if message.ends_with('\n') {
        message
    } else {
        format!("{message}\n")
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other CLI error is reported as text from the service command"
)]
fn process_error(error: crate::Error) -> ServiceError {
    match error {
        crate::Error::Config(error) => ServiceError::Config(error),
        crate::Error::Io(source) => ServiceError::Io(source),
        crate::Error::Task(error) => ServiceError::Task(error),
        crate::Error::Signal(errno) => ServiceError::Account(errno),
        other => ServiceError::Node(other.to_string()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("child process id is outside the platform range: {0}")]
    Pid(#[from] std::num::TryFromIntError),
    #[error("Service file already exists: {}\nDelete it first if you want to regenerate.", .0.display())]
    Exists(PathBuf),
    #[error("{0}")]
    Edited(String),
    #[error("invalid service path: {}", .0.display())]
    Path(PathBuf),
    #[error(
        "cannot load {} from the service: systemd cannot represent this path in EnvironmentFile",
        path.display()
    )]
    EnvironmentFile { path: PathBuf },
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
    #[error(
        "a RivetHub node is already running (pid {pid}). Stop it with `rh stop`, then run `rh service start` again."
    )]
    Running { pid: u32 },
    #[error("could not read the running node: {0}")]
    Node(String),
}
