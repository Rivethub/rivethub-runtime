use kernel::memory::MemoryStore;
use rmcp::model::CallToolResult;

use super::Import;
use super::Sessions;
use super::files::Kind;
use super::noun;
use crate::Error;
use kernel::memory::share::Findings;
use kernel::memory::share::ListedSession;
use kernel::memory::share::OpenedBundle;
use kernel::memory::share::SanitizedBundle;

#[derive(Debug)]
enum Verdict {
    Findings(Findings),
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
        findings,
        withheld,
        unreviewed,
        sessions: listing,
        authors,
        batches,
    } = bundle;
    let verdict = match (findings, withheld) {
        (Some(findings), _) => Verdict::Findings(findings),
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
            Verdict::Findings(findings) => (refusal(&findings, &path), crate::refused),
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
        Verdict::Findings(Findings {
            count,
            sessions,
            summary,
        }) => {
            return Err(Error::BundleHeld {
                path,
                count,
                sessions,
                summary,
            });
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

fn refusal(findings: &Findings, path: &std::path::Path) -> String {
    format!(
        "refused: {} in {} ({}); nothing written. Ask the user to run `rh memory ingest-bundle {}` in a terminal\n",
        noun(findings.count, "finding", "findings"),
        noun(findings.sessions, "session", "sessions"),
        findings.summary,
        path.display()
    )
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
