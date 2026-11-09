mod bundle;
mod cleanup;
mod export;
mod io;
mod merge;
mod parse;
mod recompile;
mod run;
mod sanitize;
mod target;
mod titles;

use config::Environment;
use tokio_util::sync::CancellationToken;

use crate::Report;

pub use export::MemoryExporter;

pub async fn execute(
    args: &[String],
    env: &Environment,
    shutdown: &CancellationToken,
    stdout_is_terminal: bool,
) -> Report {
    let notice = env.legacy_notice(config::POSTGRES_URL_VARIABLE, "RIVETOS_PG_URL");
    let result = match parse::parse(args) {
        Ok(None) => Ok(Report::success(help())),
        Ok(Some(command)) => run::run(command, env, shutdown, stdout_is_terminal).await,
        Err(error) => Err(error),
    };
    attach(notice, result)
}

fn attach(notice: Option<String>, result: Result<Report, crate::Error>) -> Report {
    let mut report = match result {
        Ok(report) => report,
        Err(error) => Report::failure(&error),
    };
    if let Some(notice) = notice {
        report.stderr = format!("warning: {notice}\n{}", report.stderr);
    }
    report
}

fn help() -> String {
    "Usage: rh memory <command>\n\
Commands:\n\
  queue-status [--json]\n\
  retry-failed --task <identifier> [--task <identifier>] [--error <substring>] [--limit <n>] [--dry-run] [--json]\n\
  requeue --task <identifier> [--limit <n>] [--dry-run] [--json]\n\
  backfill-tool-synth [--limit <n>] [--dry-run] [--json]\n\
  export [--out <file>] [--since <iso>] [--sqlite <file>]\n\
  bundle --out <file> [--session <conversation id>]... [--sqlite <file>]\n\
  sanitize <file> --out <file> [--mapping <file>]\n\
  ingest-bundle <file> [--force] [--dry-run]\n\
  import <file> [--dry-run] [--sqlite <file>]\n\
  backfill-titles [--dry-run] [--json] [--sqlite <file>] [--model <name> --model-url <url> [--concurrency <n>]]\n\
  reembed [--dry-run] [--sqlite <file>]\n\
  consolidate-wiki [--limit <n>] [--dry-run] [--json]\n\
  recompile-wiki --since <iso> [--min-bytes <n>] [--slug <slug>]... [--dry-run] [--json]\n\
  cleanup (--dry-run [--database <url>] | --propose --database <url>) --out <dir> --model-url <url> --model <name> [--batch <n>] [--concurrency <n>] [--timeout <seconds>]\n\
  merge-store --from <url> --owner <user> [--into <url>] [--moves <file>] [--dry-run] [--json]\n\
sanitize writes a copy of a raw bundle with secrets, tool arguments and tool results scrubbed and stamps it sanitized, and writes a report of every change beside it (<out without .json>.report.md) that never shows the replaced text. Secret patterns (keys, tokens, JWTs, private keys, connection-string passwords, .env secrets) become [REDACTED:<kind>]. The mapping file (--mapping, else memory.sanitize.mapping, else ~/.config/rivethub/redactions.toml when it exists) is a list of [[rule]] tables, each with literal = \"<text>\" or regex = '<pattern>', and action = \"map\" with as = \"<placeholder>\", or action = \"redact\", which writes [REDACTED]; rules apply in file order to every text field, to JSON keys, and to the node names, and the author name stays. It refuses a bundle that is already sanitized. bundle writes the node owner's sessions, or only the conversations each --session names, to a raw (not sanitized) version 2 bundle another RivetHub can ingest: each session carries its author, node and original dates, and the manifest carries a digest of the whole bundle. It reads --sqlite when given, else Postgres (RIVETHUB_PG_URL, else memory.postgres), else memory.sqlite.path, and replaces --out only once the whole file is written. ingest-bundle posts that file, version 1 or 2, to this node's gateway, or to memory.capture.gateway when that is set, and the receiving node records every session as the user it authenticated, in a conversation of its own for each importing user, keyed by a hash of that user and the origin node and conversation (never one it captured itself), with the session's original dates and its origin (author, node, conversation) kept in the conversation settings. It refuses a raw bundle unless --force is given, and it scans a sanitized bundle for secret patterns again and refuses it when one still matches, because the stamp alone proves nothing; --dry-run lists each session, its message count and its author and writes nothing. Postgres commands read RIVETHUB_PG_URL. export, import and backfill-titles use --sqlite or memory.sqlite.path when no Postgres URL is set. reembed runs on a SQLite memory file only. backfill-titles --model asks that model for a short title for every summarized session whose title is a placeholder or weak, on Postgres only. cleanup reads memory read-only, plans the topic tree, filings and wiki folds with the model, checkpoints every stage in --out and writes report.md there; rerun it to resume. --propose also writes the plan as one tidy batch to the database --database names, never to the live memory database. recompile-wiki queues one recompile-wiki job per wiki page at or over --min-bytes (default 61440) or per --slug: the wiki-builder worker restores the page's real History entries recorded since --since, rewrites its oversized article and sections within budget with the wiki model and moves the rest to archive/<slug>.md. merge-store copies another memory database into this one (--into, else RIVETHUB_PG_URL) in one transaction: its rows take --owner and stay private, --moves lists `<conversation id> <user>` lines that belong to someone else, ids are kept, and a rerun adds only what the source gained.\n"
        .to_owned()
}
