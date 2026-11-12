use std::fmt::Write as _;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore;
use kernel::memory::SessionDigest;
use kernel::memory::share;
use kernel::memory::share::find::DEFAULT_SESSIONS;
use kernel::memory::share::find::MAX_SESSIONS;
use kernel::memory::share::find::Matched;
use kernel::memory::share::find::Ranked;
use kernel::memory::share::find::Search;
use rmcp::model::CallToolResult;

use super::Find;
use super::Sessions;
use super::noun;
use crate::Error;
use crate::catalog::ToolInput as _;

pub(crate) async fn find(
    store: &dyn MemoryStore,
    kit: &Sessions,
    args: Find,
    zone: chrono_tz::Tz,
) -> Result<CallToolResult, Error> {
    let query = args.query.trim().to_owned();
    if query.is_empty() {
        return Err(Error::Invalid(
            "query must name what the sessions were about".into(),
        ));
    }
    let wanted =
        crate::memory::limit(Find::NAME, args.limit, DEFAULT_SESSIONS, MAX_SESSIONS)?.get();
    let tag = args
        .tag
        .map(|tag| tag.parse())
        .transpose()
        .map_err(|error: kernel::memory::TagLiteralError| Error::Invalid(error.to_string()))?;
    let outcome = store
        .search(share::find::query(
            Search {
                text: query.clone(),
                agent: args.agent,
                tag,
                since: args.since,
                before: args.before,
            },
            wanted,
        ))
        .await?;
    let ranked = share::find::group(&outcome.results, |text| kit.share.redact(text));
    let keys: Vec<String> = ranked.iter().map(|session| session.key.clone()).collect();
    let digests = if keys.is_empty() {
        Vec::new()
    } else {
        kit.share.digests(&keys).await?
    };
    let Matched { found, dropped } = share::find::matched(&ranked, &digests, wanted);
    Ok(crate::text(render(
        &query,
        &found,
        dropped,
        zone,
        |text| kit.share.redact(text),
    )?))
}

fn render(
    query: &str,
    found: &[(&Ranked, &SessionDigest)],
    dropped: usize,
    zone: chrono_tz::Tz,
    redact: impl Fn(&str) -> String,
) -> Result<String, Error> {
    let mut out = String::new();
    if found.is_empty() {
        writeln!(out, "No sessions match \"{query}\".")?;
    }
    let day = |at: DateTime<Utc>| at.with_timezone(&zone).format("%Y-%m-%d").to_string();
    for (index, (ranked, digest)) in found.iter().enumerate() {
        let imported = digest.imported_from.is_some();
        write!(
            out,
            "{}. {}  \"{}\"  {}  {} to {}  {}  best {:.2} ({})",
            index + 1,
            digest.conversation_id,
            crate::provenance::own_text(
                imported,
                &digest
                    .title
                    .as_deref()
                    .map_or_else(|| "untitled".to_owned(), &redact)
            ),
            crate::provenance::own_text(imported, &digest.agent),
            day(digest.first),
            day(digest.last),
            noun(digest.messages, "message", "messages"),
            ranked.best,
            noun(ranked.hits, "hit", "hits"),
        )?;
        if let Some(author) = &digest.imported_from {
            write!(
                out,
                "  from {} (imported)",
                crate::provenance::own_text(true, author)
            )?;
        }
        out.push('\n');
        for excerpt in &ranked.excerpts {
            if imported {
                writeln!(
                    out,
                    "   > {}",
                    kernel::memory::escape_hidden(&excerpt.replace(['\n', '\r'], " "))
                )?;
            } else {
                writeln!(out, "   > {excerpt}")?;
            }
        }
    }
    if dropped > 0 {
        writeln!(
            out,
            "left out {} from sessions this node cannot export (trashed or not yours)",
            noun(dropped, "hit", "hits")
        )?;
    }
    if !found.is_empty() {
        writeln!(
            out,
            "pass the conversation ids to share to session_sanitize, each with decision approve or redact; leave out the ones to deny"
        )?;
    }
    Ok(out)
}
