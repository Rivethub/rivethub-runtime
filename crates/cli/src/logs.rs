use config::Environment;
use tokio_util::sync::CancellationToken;

use crate::Error;
use crate::Exit;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;
use crate::service::Scope;

const DEFAULT_LINES: u32 = 50;

struct LogsCommand {
    agent: Option<String>,
    follow: bool,
    lines: u32,
    since: Option<String>,
    level: Option<String>,
    grep: Option<String>,
    json: bool,
}

enum Parsed {
    Help,
    Unknown(String),
    Command(LogsCommand),
}

pub async fn execute(
    scope: Scope,
    args: &[String],
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    match parse(args)? {
        Parsed::Help => Ok(Report::success(help())),
        Parsed::Unknown(token) => Ok(unknown(&token)),
        Parsed::Command(command) => run(scope, command, env, shutdown).await,
    }
}

async fn run(
    scope: Scope,
    command: LogsCommand,
    env: &Environment,
    shutdown: &CancellationToken,
) -> Result<Report, Error> {
    let arguments = journal_args(&command);
    match scope
        .read_journal(&arguments, command.follow, env, shutdown)
        .await
    {
        Ok(report) => Ok(report),
        Err(error) => Ok(Report::failure(&Error::from(error))),
    }
}

fn parse(args: &[String]) -> Result<Parsed, Error> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Parsed::Help);
    }
    let mut scan = Scan::new(args);
    let mut command = LogsCommand {
        agent: None,
        follow: true,
        lines: DEFAULT_LINES,
        since: None,
        level: None,
        grep: None,
        json: false,
    };
    while scan.current().is_some() {
        if take_flag(&mut scan, &mut command)? {
            continue;
        }
        let Some(token) = scan.current().map(str::to_owned) else {
            break;
        };
        if token.starts_with('-') {
            return Ok(Parsed::Unknown(token));
        }
        if command.agent.is_some() {
            return Err(Error::OneAgent);
        }
        command.agent = Some(token);
        scan.bump();
    }
    Ok(Parsed::Command(command))
}

fn take_flag(scan: &mut Scan<'_>, command: &mut LogsCommand) -> Result<bool, Error> {
    if scan.flag("-f") || scan.flag("--follow") {
        command.follow = true;
        return Ok(true);
    }
    if scan.flag("--no-follow") {
        command.follow = false;
        return Ok(true);
    }
    if scan.flag("--json") {
        command.json = true;
        return Ok(true);
    }
    if scan.flag("--bare-metal") {
        return Ok(true);
    }
    match scan.option("--lines", Some("-n")) {
        Taken::Value(value) => {
            command.lines = line_count(&value)?;
            return Ok(true);
        }
        Taken::Missing => return Err(Error::LinesInteger),
        Taken::Absent => {}
    }
    match scan.option("--since", None) {
        Taken::Value(value) => {
            command.since = Some(value);
            return Ok(true);
        }
        Taken::Missing => return Err(Error::JournalSince),
        Taken::Absent => {}
    }
    match scan.option("--level", None) {
        Taken::Value(value) => {
            syslog(&value)?;
            command.level = Some(value);
            return Ok(true);
        }
        Taken::Missing => return Err(Error::LevelRequired),
        Taken::Absent => {}
    }
    match scan.option("--grep", Some("-g")) {
        Taken::Value(value) => {
            command.grep = Some(value);
            return Ok(true);
        }
        Taken::Missing => return Err(Error::GrepRequired),
        Taken::Absent => {}
    }
    Ok(false)
}

fn journal_args(command: &LogsCommand) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(agent) = &command.agent {
        args.push("-t".to_owned());
        args.push(agent.clone());
    }
    args.push("-n".to_owned());
    args.push(command.lines.to_string());
    if let Some(since) = &command.since {
        args.push("--since".to_owned());
        args.push(since.clone());
    }
    if let Some(level) = &command.level {
        args.push("-p".to_owned());
        args.push(level.clone());
    }
    if let Some(grep) = &command.grep {
        args.push("-g".to_owned());
        args.push(grep.clone());
    }
    if command.json {
        args.push("-o".to_owned());
        args.push("json".to_owned());
    }
    if command.follow {
        args.push("-f".to_owned());
    }
    args.push("--no-pager".to_owned());
    args
}

fn line_count(text: &str) -> Result<u32, Error> {
    text.parse::<u32>().map_err(|_| Error::LinesInteger)
}

fn syslog(text: &str) -> Result<(), Error> {
    match text {
        "emerg" | "alert" | "crit" | "err" | "warning" | "notice" | "info" | "debug" | "0"
        | "1" | "2" | "3" | "4" | "5" | "6" | "7" => Ok(()),
        _ => Err(Error::LevelUnknown(text.to_owned())),
    }
}

fn unknown(token: &str) -> Report {
    Report {
        stdout: String::new(),
        stderr: format!("Unknown option: {token}\n{}", help()),
        exit: Exit::Failure,
        stdout_raw: Vec::new(),
    }
}

fn help() -> String {
    "Usage: rh logs [agent] [options]\n\
  -f, --follow       follow the journal (default)\n\
      --no-follow    print the selected lines and exit\n\
  -n, --lines <n>    number of lines (default 50)\n\
      --since <time> journalctl --since value\n\
      --level <name> syslog level\n\
  -g, --grep <text>  filter lines\n\
      --json         journal JSON\n\
      --bare-metal   read the service journal (docker is not used)\n\
  -h, --help         show this help\n"
        .to_owned()
}
