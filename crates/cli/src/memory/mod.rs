mod bundle;
mod chosen;
mod cleanup;
mod dropbox;
mod export;
mod hold;
mod inbox;
mod io;
mod merge;
mod parse;
mod recompile;
mod run;
mod sanitize;
mod share;
mod store;
mod target;
mod titles;

use config::Environment;
use tokio_util::sync::CancellationToken;

use crate::Report;

pub use dropbox::DropError;
pub use export::MemoryExporter;
pub use share::SealedFile;
pub use share::ShareError;
pub use share::Shared;
pub use share::code_line;
pub use share::read_bundle_file;
pub use share::seal_bytes;
pub use share::seal_file;
pub use share::upload;
pub use store::SessionStore;

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
  sanitize <file> --out <file> [--mapping <file>] [--decide [--block]]\n\
  sanitize --session <id>... [--redact-term <id>=<term>]... [--drop-tool-outputs <id>]... --out <file> [--mapping <file>] [--decide [--block]] [--sqlite <file>]\n\
  sanitize [<bundle>] --resolve <report> [--mapping <file>] [--learn]\n\
  ingest-bundle <file> [--force] [--dry-run]\n\
  share <sanitized bundle> [--server <host>] [--unsigned] [--flags <n>]\n\
  receive <share code> [--dry-run]\n\
  key show\n\
  trust add <name> <key> | trust list | trust remove <name> [<key>]\n\
  import <file> [--dry-run] [--sqlite <file>]\n\
  backfill-titles [--dry-run] [--json] [--sqlite <file>] [--model <name> --model-url <url> [--concurrency <n>]]\n\
  reembed [--dry-run] [--sqlite <file>]\n\
  consolidate-wiki [--limit <n>] [--dry-run] [--json]\n\
  recompile-wiki --since <iso> [--min-bytes <n>] [--slug <slug>]... [--dry-run] [--json]\n\
  cleanup (--dry-run [--database <url>] | --propose --database <url>) --out <dir> --model-url <url> --model <name> [--batch <n>] [--concurrency <n>] [--timeout <seconds>]\n\
  merge-store --from <url> --owner <user> [--into <url>] [--moves <file>] [--dry-run] [--json]\n\
sanitize is best-effort: it reliably catches known vendor tokens, env, JSON, YAML, XML, SQL and code assignments to password- and token-named keys, name/value pairs with such names, URL credentials, PEM and DER private keys, base64 blobs that decode to any of those, and mapped terms; it can miss unusual code forms, vendor shapes it has no rule for, and passwords written as bare words in prose or with spaces outside quotes, so read the report before a bundle leaves the machine (the share skill will show it first once it lands). It prints a review summary after the totals: changes by rule, the fields it never scans, and the report path. It writes a copy of a raw bundle with secrets scrubbed from every text field (title, settings, content, tool results, tool arguments, metadata, session key, agent, channel, task id, tool names, node names, the conversation id, the author and manifest.exported_by.user and .node), stamps it sanitized, rewrites each event id as an HMAC of the raw id under a local key, and writes a report of every change beside it (<out without .json>.report.md) that never shows the text the secrets and mapping passes replaced. The secret rules are data in the capture crate: 67 RivetHub rules in rivethub.toml and the default gitleaks rules in gitleaks.toml (v8.30.1, pinned by digest, MIT; path-only rules, generic-api-key and private-key are not loaded); only the secret value becomes [REDACTED:<rule id>], so a name and its separator stay. Names are judged by family: a password-family key (password, passwd, pwd, passphrase, pin, secret, private_key, ssh_key, secret_value, or a key ending in one of them or in _pass) redacts any string value that is not a stopword (null, none, true, changeme and the like) or a template ({{ x }}, {NAME}, ${VAR}, $VAR, %VAR%, <name>, ***); a token-family key (token, api_key, apikey, access_key, secret_key, app_key, account_key, signing_key, encryption_key, master_key, auth_key, auth, credentials, credential, creds, cookie, session_cookie, license_key, routing_key, write_key, client_key, consumer_key, unseal_key, token_value, or a key ending in one of them) redacts a value of 12 or more characters with Shannon entropy 3.0 or more, or one with a known vendor prefix; a key ending in _key, or exactly key or pass, outside a short never-list (session_key, public_key, cache_key and the like) redacts a value of 20 or more characters with entropy 3.5 or more that holds a digit and no space; a key ending in _id, _count, _at, _ref, _name, _url or any other word is never a secret name; JSON numbers, booleans, null, code expressions, templates and version ranges under token keys are never secrets, and the separators are =, :, => and :=. A quoted value runs to its closing quote; a bare one ends at whitespace or , ; & ) ] }. In a JSON object a string under a secret-family key, at any depth, becomes [REDACTED:secret_key]; numbers, booleans and null stay typed. In text, a JSON or YAML value that opens a bracket and continues over the next lines (pretty-printed terraform output -json) is read to its closing bracket, a YAML block scalar to its last indented line, and the value of a name/value pair whose name is in a family ({\"name\": \"DB_PASSWORD\", \"value\": \"x\"}, or - name: DB_PASSWORD then value: x, as in kubectl env, ECS task definitions and HAR headers and params) becomes [REDACTED:name_value_pair]; every value in a HAR cookies array, and the --body of gh secret set, are redacted too. A ${NAME:-default}, ${NAME:?message} or ${NAME:default} expansion is a template and stays whole; a vendor token inside its default is still redacted. Base64 text of 40 or more characters, wrapped or not, gzip or nested once, is scanned decoded and redacted whole on a hit or when it is a DER private key; decoding stops at 8 MiB of decoded bytes per message, 64 MiB per bundle and two nested layers, and a blob past that budget becomes [REDACTED:undecoded_blob] without being decoded, in sanitize and in ingest-bundle's scan. That scan runs over messages in parallel, takes every inflated gzip byte from the one 64 MiB bundle budget before inflating it (a plain base64 decode is charged after it, so each worker thread can decode one blob of up to about 6 MiB past the budget), and scans again in order any message that ran short, so it decodes at most 128 MiB plus one such blob per worker thread however many messages a bundle holds and decides exactly as an in-order scan would. Each event id becomes HMAC(event_id, raw node, raw conversation id, raw session key, raw event id) and origin.identity becomes HMAC(conversation, raw node, raw conversation id), which the receiver keys the conversation by. Fingerprints in the report, the policy hash and the event ids are HMAC-SHA256 under a 32-byte key created once at $XDG_STATE_HOME/rivethub/sanitize.key (else ~/.local/state/rivethub/sanitize.key, mode 0600), never shipped: the same raw bundle gives the same ids on the same machine, a copied key gives the same ids on another machine, and a new key makes the receiver see new ids, so re-imports duplicate. A task id that matches a secret rule becomes a UUID derived from the key; task ids and the author are never mapped. The mapping file (--mapping, else memory.sanitize.mapping, else ~/.config/rivethub/redactions.toml when it exists) is a list of [[rule]] tables, each with literal = \"<text>\" or regex = '<pattern>', and action = \"map\" with as = \"<placeholder>\", or action = \"redact\", which writes [REDACTED]; rules apply to the same fields and to JSON keys, an earlier rule wins where matches overlap (with the rules abc then zab, zabc becomes z<abc's placeholder>), two keys that scrub to the same text keep both values with a #2 suffix in an order that reveals nothing about the input, and dates stay. The stopwords and templates of the names table allow values for RivetHub rules only; gitleaks' own allowlist applies to gitleaks rules only. Text a rule already replaced is protected, and a match that runs into a placeholder is trimmed to the text around it. It refuses a bundle that is already sanitized. With --session instead of a bundle file it reads those conversations of the node owner straight from the store (--sqlite, else Postgres, else memory.sqlite.path, as bundle does) and never writes a raw bundle; an id that is unknown, trashed or another owner's is refused by name and nothing is written. --redact-term <id>=<term> turns that literal text (case-sensitive, in the fields and JSON keys mapping rules cover) into [REDACTED:session_term] in that session only, and --drop-tool-outputs <id> turns every tool result of that session into [REDACTED:drop_tool_outputs] while its tool names and arguments are sanitized as usual; both name a session given with --session, and the report counts them under the rules session_term and drop_tool_outputs without the term. --decide then checks GET <url>/v1/decide/info on the decision model at memory.sanitize.decide.url (concurrency 1 to 8, default 8) and, once the re-check described below has run, scores every chunk of text left after those passes (chunks of at most 1200 characters on line boundaries; [tool call] lines, placeholders, identifiers (UUIDs, hex digests, base64url ids, times, metadata uuid/requestId/session fields and _id/_uuid/_path values that are ids) and bare tokens under 16 characters with no capitalized word, none of @ / = , : ;, no non-ASCII letter, no mix of 5 or more letters and digits, and no IP, phone or host-name shape are trivial and skipped); it follows no redirect, reads at most 1 MiB per answer and needs two probabilities summing to 1. It lists the highest-scoring texts of each session (top, default 20; never under threshold, default 0.3) in the report as flags to review; a flag covers every exact occurrence of its text that the review found in the bundle, in any session, inside other flagged text, JSON keys, tool names, channels and task ids too, but never inside a placeholder, and shows its score, its places (<event id>:<field>, using the rewritten event id) and its text, so never share the report; if the model cannot be reached the run fails and writes nothing. The bundle stamp records the review without saying where any flag is: the model, the calls, each flag's number and mark, and a seal under the local key over them and unresolved_flags; the places and the pre-review text stay in <out without .json>.review.local.json (mode 0600, sealed, never shared). Where flags overlap, each byte takes redact, then withheld (--block, any covering flag open), then map, then keep. The review is advisory: the bundle keeps each flagged text and its stamp counts the flags not yet marked. With --block or memory.sanitize.decide.block = true the bundle holds [FLAGGED:<n>] at every place of each flag until it is marked (a task id becomes a key-derived UUID instead), and ingest refuses it while any flag is open. --resolve applies the marks written in the report (keep, redact, or map <term> as <placeholder>; a word-like term matches whole words; a placeholder may not be a [FLAGGED or [REDACTED form or look like a secret) by rendering each field that holds a place again from its pre-review text in the local review file, refuses a local file sealed under another machine's key, any edit to the bundle since the last run, a flag whose text was edited or whose hidden copy does not match the local file, and a mark for a flag the bundle lacks, keeps a flag whose section is gone open, rewrites the report, and offers each map mark as a regex mapping rule bounded by Unicode word edges ((?u:\\b), so Tom spares Tomás), which --learn appends to the mapping file (terms of 3 or more characters) after checking the file parses and creating its folder, before anything is written; mark some flags now and the rest later. bundle writes the node owner's sessions, or only the conversations each --session names, to a raw (not sanitized) version 2 bundle another RivetHub can ingest: each session carries its author, node and original dates, and the manifest carries a digest of the whole bundle. It reads --sqlite when given, else Postgres (RIVETHUB_PG_URL, else memory.postgres), else memory.sqlite.path, and replaces --out only once the whole file is written. ingest-bundle posts that file, version 1 or 2, to this node's gateway, or to memory.capture.gateway when that is set, and the receiving node records every session as the user it authenticated, in a conversation of its own for each importing user, keyed by a hash of that user and the origin node and conversation (never one it captured itself), with the session's original dates and its origin (author, node, conversation) kept in the conversation settings. It refuses a raw bundle unless --force is given, and it scans every field of a sanitized bundle for secret patterns again, event ids, task ids, the author and the exporter included, and refuses it when one still matches, because the stamp alone proves nothing (--force overrides that scan too and the output says how many matches were forced); a key that is exactly a placeholder is not a secret name. It also refuses a bundle that still withholds flagged chunks unless --force is given, and a bundle whose unresolved_flags disagrees with its review table. sanitize runs that same scan on its own output before it writes anything, redacts whatever it still finds (each such span is a sanitizer bug and the report counts them) and refuses to write a bundle the scan would refuse, so sanitize's own output always passes, with --decide and --block and after --resolve too; [FLAGGED:<n>] and [REDACTED:flagged] are placeholders that scan knows, and a [FLAGGED:<n>] the raw session already holds gets one more colon ([FLAGGED::<n>], digits kept) and is counted as escaped, so it never reads as a withheld flag. --resolve runs that scan with the same re-check after applying the marks, because a mark can expose a secret shape, and prints each span it redacted. It cannot re-apply the sender's mapping, which never ships. --dry-run lists each session, its message count and its author and writes nothing. Postgres commands read RIVETHUB_PG_URL. export, import and backfill-titles use --sqlite or memory.sqlite.path when no Postgres URL is set. reembed runs on a SQLite memory file only. backfill-titles --model asks that model for a short title for every summarized session whose title is a placeholder or weak, on Postgres only. cleanup reads memory read-only, plans the topic tree, filings and wiki folds with the model, checkpoints every stage in --out and writes report.md there; rerun it to resume. --propose also writes the plan as one tidy batch to the database --database names, never to the live memory database. recompile-wiki queues one recompile-wiki job per wiki page at or over --min-bytes (default 61440) or per --slug: the wiki-builder worker restores the page's real History entries recorded since --since, rewrites its oversized article and sections within budget with the wiki model and moves the rest to archive/<slug>.md. merge-store copies another memory database into this one (--into, else RIVETHUB_PG_URL) in one transaction: its rows take --owner and stay private, --moves lists `<conversation id> <user>` lines that belong to someone else, ids are kept, and a rerun adds only what the source gained.\n"
        .to_owned()
}
