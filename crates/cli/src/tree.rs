use std::ffi::OsString;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

use crate::{Exit, Report};

#[derive(Debug, Parser)]
#[command(
    name = "rh",
    about = "RivetHub operator commands",
    disable_help_subcommand = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Update this node or the mesh")]
    Update(crate::update::UpdateArgs),
    #[command(about = "Apply or inspect database migrations")]
    Db(crate::db::DbArgs),
    #[command(about = "Run the composed gateway service")]
    Gateway {
        #[arg(long)]
        check: bool,
        #[arg(short = 'c', long = "config")]
        config: Option<PathBuf>,
    },
    #[command(about = "Run a memory worker for one role")]
    Worker {
        #[arg(long)]
        role: workers::ProcessRole,
        #[arg(long)]
        check: bool,
        #[arg(short = 'c', long = "config")]
        config: Option<PathBuf>,
    },
    #[command(about = "Run the MCP sidecar")]
    Mcp {
        #[arg(long)]
        check: bool,
        #[arg(short = 'c', long = "config")]
        config: Option<PathBuf>,
    },
    #[command(about = "Manage the systemd service")]
    Service {
        #[command(subcommand)]
        command: crate::service::ServiceCommand,
    },
    #[command(about = "Manage the certificate authority")]
    Ca(crate::ca::CaArgs),
    #[command(about = "Manage holder certificates")]
    Cert {
        #[command(subcommand)]
        command: crate::cert::CertCommand,
    },
    #[command(about = "Register and inspect mesh nodes")]
    Mesh(crate::mesh::MeshArgs),
    #[command(about = "Print the RivetHub version")]
    Version,
    #[command(about = "Inspect and validate configuration")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    #[command(about = "Check this node without applying migrations")]
    Doctor {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Report gateway health")]
    Status,
    #[command(about = "Signal the running instance to stop")]
    Stop,
    #[command(about = "Run a standalone node on SQLite")]
    Local {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        no_lan: bool,
        #[arg(
            long,
            help = "Start the node in the background unless one already answers its health check"
        )]
        ensure: bool,
        #[arg(
            long,
            requires = "ensure",
            default_value_t = 2,
            value_parser = clap::value_parser!(u64).range(1..=300),
            help = "Seconds --ensure waits for the node to answer"
        )]
        wait: u64,
    },
    #[command(about = "Administer memory", disable_help_flag = true)]
    Memory {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(about = "Capture harness sessions into memory")]
    Capture {
        #[command(subcommand)]
        command: crate::capture_hook::CaptureCommand,
    },
    #[command(about = "Show and wire the coding harnesses on this machine")]
    Harness {
        #[command(subcommand)]
        command: crate::harness::HarnessCommand,
    },
    #[command(about = "Read the service journal", disable_help_flag = true)]
    Logs {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    #[command(hide = true, about = "Supervise the configured service roots")]
    Start {
        #[arg(short = 'c', long = "config")]
        config: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Print the resolved configuration path")]
    Path,
    #[command(about = "Show a configuration summary without secrets")]
    Show,
    #[command(about = "Validate configuration and print findings")]
    Validate { path: Option<PathBuf> },
}

impl Cli {
    pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Self, Report> {
        let args: Vec<_> = args.into_iter().collect();
        let service = args.get(1).is_some_and(|arg| arg == "service");
        let mesh = args.get(1).is_some_and(|arg| arg == "mesh");
        match Self::try_parse_from(args) {
            Ok(cli) => Ok(cli),
            Err(error) if error.use_stderr() => {
                let text = if error.kind() == clap::error::ErrorKind::InvalidSubcommand {
                    match error.get(clap::error::ContextKind::InvalidSubcommand) {
                        Some(_) if mesh => {
                            return Err(Report::success(crate::mesh::help()));
                        }
                        Some(name) if service => {
                            let mut tree = Self::command();
                            let help = tree
                                .find_subcommand_mut("service")
                                .map(clap::Command::render_help);
                            match help {
                                Some(help) => format!("Unknown action: {name}\n{help}\n"),
                                None => error.to_string(),
                            }
                        }
                        Some(name) => format!("Unknown command: {name}\n{}", Self::help()),
                        None => error.to_string(),
                    }
                } else {
                    error.to_string()
                };
                Err(Report {
                    stdout: String::new(),
                    stderr: text,
                    exit: Exit::Failure,
                    stdout_raw: Vec::new(),
                })
            }
            Err(error) => Err(Report::success(error.to_string())),
        }
    }

    pub fn help() -> String {
        format!("{}\n", Self::command().render_help())
    }
}
