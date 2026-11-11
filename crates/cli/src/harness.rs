use capture::harnesses::HarnessSetupService;
use chrono::Utc;
use clap::Args;
use clap::Subcommand;
use config::Environment;
use config::Paths;
use kernel::harness_setup::CaptureHistory;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessStatus;

use crate::Error;
use crate::Report;

#[derive(Debug, Subcommand)]
pub enum HarnessCommand {
    #[command(
        about = "Show which harnesses are found, how they capture and whether recall is wired"
    )]
    Status {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Wire a harness's capture hook and recall MCP entry to rh")]
    Connect(ConnectArgs),
    #[command(about = "Remove rh's hook and MCP entry from a harness")]
    Disconnect(ConnectArgs),
}

#[derive(Debug, Args)]
pub struct ConnectArgs {
    pub id: String,
    #[arg(
        long,
        conflicts_with = "recall",
        help = "Only the capture hook or plug-in"
    )]
    pub capture: bool,
    #[arg(long, help = "Only the recall MCP entry")]
    pub recall: bool,
    #[arg(long, help = "Print the diff and write nothing")]
    pub dry_run: bool,
}

impl ConnectArgs {
    fn part(&self) -> Option<ConnectPart> {
        match (self.capture, self.recall) {
            (true, _) => Some(ConnectPart::Capture),
            (false, true) => Some(ConnectPart::Recall),
            (false, false) => None,
        }
    }
}

pub(crate) async fn run(command: HarnessCommand, env: &Environment) -> Result<Report, Error> {
    let backups = Paths::new(env)?.hub_home().join("backups");
    let executable = tokio::task::spawn_blocking(std::env::current_exe).await??;
    let service = HarnessSetupService::new(env, executable, backups);
    match command {
        HarnessCommand::Status { json } => {
            let statuses = tokio::task::spawn_blocking(move || {
                let now = Utc::now();
                service
                    .probe_now()
                    .into_iter()
                    .map(|probe| probe.into_status(CaptureHistory::Unknown, now))
                    .collect::<Vec<_>>()
            })
            .await?;
            if json {
                Ok(Report::success(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&statuses)?
                )))
            } else {
                Ok(Report::success(table(&statuses)))
            }
        }
        HarnessCommand::Connect(args) => {
            let outcome = tokio::task::spawn_blocking(move || {
                service.connect_now(&args.id, args.part(), args.dry_run, Utc::now())
            })
            .await??;
            Ok(Report::success(change_text(&outcome, "connected")))
        }
        HarnessCommand::Disconnect(args) => {
            let outcome = tokio::task::spawn_blocking(move || {
                service.disconnect_now(&args.id, args.part(), args.dry_run, Utc::now())
            })
            .await??;
            Ok(Report::success(change_text(&outcome, "disconnected")))
        }
    }
}

fn change_text(outcome: &ConnectOutcome, done: &str) -> String {
    if outcome.diff.is_empty() {
        return "Nothing to change.\n".to_owned();
    }
    let separator = if outcome.diff.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let closing = if outcome.backup_path.is_empty() {
        "Dry run: nothing was written.\n".to_owned()
    } else {
        format!("Backed up to {}\n{done}.\n", outcome.backup_path)
    };
    format!("{}{separator}{closing}", outcome.diff)
}

fn table(statuses: &[HarnessStatus]) -> String {
    statuses.iter().map(block).collect()
}

fn block(status: &HarnessStatus) -> String {
    let found = match &status.path {
        Some(path) if status.found => format!("found at {path}"),
        Some(_) | None => "not found".to_owned(),
    };
    let connect = if status.can_connect {
        format!("  connect: rh harness connect {}\n", status.id)
    } else {
        String::new()
    };
    format!(
        "{} ({}): {found}\n  capture ({}): {}\n  recall: {}\n{connect}",
        status.name, status.id, status.capture.method, status.capture.detail, status.recall.detail
    )
}
