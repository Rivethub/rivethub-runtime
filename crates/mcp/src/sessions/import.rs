use kernel::memory::MemoryStore;
use rmcp::model::CallToolResult;

use super::Import;
use super::Sessions;
use super::files::Kind;
use super::noun;
use crate::Error;
use kernel::memory::share::ListedSession;
use kernel::memory::share::OpenedBundle;
use kernel::memory::share::SanitizedBundle;
use kernel::memory::share::Secrets;

#[derive(Debug)]
enum Verdict {
    Secrets(Secrets),
    Withheld(usize),
    Clean { unreviewed: usize },
}

pub(crate) async fn import(
    store: &dyn MemoryStore,
    kit: &Sessions,
    args: Import,
) -> Result<CallToolResult, Error> {
    let path = kit.files.readable(&args.path, Kind::Bundle).await?;
    let bundle = match kit.share.open(path.clone()).await? {
        OpenedBundle::Raw => return Err(Error::RawBundle(path)),
        OpenedBundle::Sanitized(bundle) => bundle,
    };
    let SanitizedBundle {
        secrets,
        withheld,
        unreviewed,
        sessions: listing,
        authors,
        batches,
    } = bundle;
    let verdict = match (secrets, withheld) {
        (Some(secrets), _) => Verdict::Secrets(secrets),
        (None, 0) => Verdict::Clean { unreviewed },
        (None, count) => Verdict::Withheld(count),
    };
    let sessions = sessions_from(listing.len(), &authors);
    if args.dry_run == Some(true) {
        let mut lines: Vec<String> = listing.iter().map(listed).collect();
        let (line, reply): (String, fn(String) -> CallToolResult) = match verdict {
            Verdict::Clean { unreviewed } => (
                format!(
                    "would import {sessions}{}; nothing written\n",
                    advisory(unreviewed)
                ),
                crate::text,
            ),
            Verdict::Secrets(Secrets { count, first }) => (
                format!(
                    "refused: {} remain ({first}); nothing written\n",
                    noun(count, "secret pattern match", "secret pattern matches")
                ),
                crate::refused,
            ),
            Verdict::Withheld(count) => (
                format!(
                    "refused: {} open; nothing written\n",
                    noun(count, "withheld flag", "withheld flags")
                ),
                crate::refused,
            ),
        };
        lines.push(line);
        return Ok(reply(lines.concat()));
    }
    let unreviewed = match verdict {
        Verdict::Clean { unreviewed } => unreviewed,
        Verdict::Secrets(Secrets { count, first }) => {
            return Err(Error::BundleSecrets { path, count, first });
        }
        Verdict::Withheld(count) => return Err(Error::BundleWithheld { path, count }),
    };
    let (mut inserted, mut skipped) = (0, 0);
    for batch in batches? {
        let captured = store.capture(batch, &kit.owner, Some(&kit.node)).await?;
        inserted += captured.inserted;
        skipped += captured.skipped;
    }
    Ok(crate::text(format!(
        "ingested {sessions}{}: {inserted} inserted, {skipped} skipped\n",
        advisory(unreviewed)
    )))
}

fn advisory(unreviewed: usize) -> String {
    if unreviewed == 0 {
        String::new()
    } else {
        format!(
            " ({} unreviewed)",
            noun(unreviewed, "advisory flag", "advisory flags")
        )
    }
}

fn sessions_from(count: usize, authors: &[String]) -> String {
    let count = noun(count, "session", "sessions");
    if authors.is_empty() {
        count
    } else {
        format!("{count} from {}", authors.join(", "))
    }
}

fn listed(session: &ListedSession) -> String {
    let dates = match session.dates {
        Some((created, updated)) => format!(
            "{} to {}",
            created.format("%Y-%m-%d"),
            updated.format("%Y-%m-%d")
        ),
        None => "undated".to_owned(),
    };
    format!(
        "{} ({}): {} from {}, {dates}\n",
        session.key,
        session.agent,
        noun(session.messages, "message", "messages"),
        session.author,
    )
}
