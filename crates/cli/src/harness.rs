use capture::harnesses::HarnessSetupService;
use capture::harnesses::SetupChange;
use chrono::Utc;
use clap::Args;
use clap::Subcommand;
use config::Config;
use config::Environment;
use config::Paths;
use kernel::harness_setup::CaptureActivity;
use kernel::harness_setup::CaptureHistory;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessStatus;
use kernel::harness_setup::SetupError;

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
            let activity = capture_activity(env).await;
            let statuses = tokio::task::spawn_blocking(move || {
                let now = Utc::now();
                service
                    .probe_now()
                    .into_iter()
                    .map(|probe| {
                        let history = match &activity {
                            Some(rows) => CaptureHistory::of(rows, &probe.session_prefix),
                            None => CaptureHistory::Unknown,
                        };
                        probe.into_status(history, now)
                    })
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
            let dry_run = args.dry_run;
            let outcome = tokio::task::spawn_blocking(move || {
                service.connect_now(&args.id, args.part(), args.dry_run, Utc::now())
            })
            .await??;
            Ok(change_report(&outcome, dry_run, "connected"))
        }
        HarnessCommand::Disconnect(args) => {
            let dry_run = args.dry_run;
            let outcome = tokio::task::spawn_blocking(move || {
                service.disconnect_now(&args.id, args.part(), args.dry_run, Utc::now())
            })
            .await??;
            Ok(change_report(&outcome, dry_run, "disconnected"))
        }
    }
}

async fn capture_activity(env: &Environment) -> Option<Vec<CaptureActivity>> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: note 174 leaves capture history off the line when the node cannot be read"
        )
    )]
    let (_, loaded) = crate::configuration::load(env).await.ok()?;
    let config = loaded.config;
    if config.sqlite_selected() {
        sqlite_activity(&config).await
    } else {
        postgres_activity(&config, env).await
    }
}

async fn sqlite_activity(config: &Config) -> Option<Vec<CaptureActivity>> {
    let path = config.memory.as_ref()?.sqlite.as_ref()?.path.clone()?;
    if path.as_path() == std::path::Path::new(":memory:") || !path.exists() {
        return Some(Vec::new());
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: note 174 leaves capture history off the line when the store cannot be read"
        )
    )]
    let store = tokio::task::spawn_blocking(move || memory_sqlite::SqliteStore::open(path))
        .await
        .ok()?
        .ok()?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: note 174 leaves capture history off the line when the store cannot be read"
        )
    )]
    return store.capture_activity_read().await.ok();
}

async fn postgres_activity(config: &Config, env: &Environment) -> Option<Vec<CaptureActivity>> {
    let url = config.memory_postgres_url(env)?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: note 174 leaves capture history off the line when the store cannot be read"
        )
    )]
    let store = memory_postgres::PostgresMemory::connect(&url).await.ok()?;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: note 174 leaves capture history off the line when the store cannot be read"
        )
    )]
    return store.read_capture_activity().await.ok();
}

fn change_report(change: &SetupChange, dry_run: bool, done: &str) -> Report {
    let Some(skipped) = &change.skipped else {
        return Report::success(change_text(&change.outcome, dry_run, done));
    };
    let done = match skipped {
        SetupError::Unsupported { part, .. } => {
            let other = match part {
                ConnectPart::Capture => ConnectPart::Recall,
                ConnectPart::Recall => ConnectPart::Capture,
            };
            format!("{done} {other} only")
        }
        SetupError::UnknownHarness(_) | SetupError::Config { .. } | SetupError::Io { .. } => {
            done.to_owned()
        }
    };
    let mut report = Report::success(change_text(&change.outcome, dry_run, &done));
    report.stderr = format!("Skipped: {skipped}\n");
    report
}

fn change_text(outcome: &ConnectOutcome, dry_run: bool, done: &str) -> String {
    if outcome.diff.is_empty() {
        return "Nothing to change.\n".to_owned();
    }
    let separator = if outcome.diff.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let closing = match (dry_run, outcome.backup_path.as_str()) {
        (true, _) => "Dry run: nothing was written.\n".to_owned(),
        (false, "") => format!("{done}.\n"),
        (false, backup) => format!("Backed up to {backup}\n{done}.\n"),
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
