use capture::sanitize::Change;
use capture::sanitize::Field;
use capture::sanitize::OpenFlags;
use kernel::memory::MemoryStore;
use rmcp::model::CallToolResult;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::Import;
use super::Sessions;
use super::files::Kind;
use super::flow::read_bundle;
use super::noun;
use crate::Error;

#[derive(Debug)]
enum Verdict {
    Secrets { count: usize, first: String },
    Withheld(usize),
    Clean { unreviewed: usize },
}

pub(crate) async fn import(
    store: &dyn MemoryStore,
    kit: &Sessions,
    args: Import,
) -> Result<CallToolResult, Error> {
    let path = kit.files.readable(&args.path, Kind::Bundle).await?;
    let bundle = read_bundle(&path).await?;
    if bundle.manifest.sanitization == Sanitization::Raw {
        return Err(Error::RawBundle(path));
    }
    let secrets = capture::sanitize::secrets(&bundle);
    let OpenFlags {
        withheld,
        unreviewed,
    } = capture::sanitize::open_flags(&bundle).map_err(capture::share::ShareError::from)?;
    let verdict = match (secrets.first(), withheld) {
        (Some(first), _) => Verdict::Secrets {
            count: secrets.len(),
            first: first.to_string(),
        },
        (None, 0) => Verdict::Clean { unreviewed },
        (None, count) => Verdict::Withheld(count),
    };
    let sessions = sessions_from(&bundle, &secrets);
    if args.dry_run == Some(true) {
        let mut lines: Vec<String> = bundle
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| listed(index, session, &secrets))
            .collect();
        let (line, reply): (String, fn(String) -> CallToolResult) = match verdict {
            Verdict::Clean { unreviewed } => (
                format!(
                    "would import {sessions}{}; nothing written\n",
                    advisory(unreviewed)
                ),
                crate::text,
            ),
            Verdict::Secrets { count, first } => (
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
        Verdict::Secrets { count, first } => {
            return Err(Error::BundleSecrets { path, count, first });
        }
        Verdict::Withheld(count) => return Err(Error::BundleWithheld { path, count }),
    };
    let (mut inserted, mut skipped) = (0, 0);
    for session in &bundle.sessions {
        let batch = capture::writer::prepare(
            &session.import_batch().map_err(Error::ImportBatch)?,
            kit.redaction.as_ref(),
        )?;
        let captured = store
            .capture(kernel_batch(batch), &kit.owner, Some(&kit.author.node))
            .await?;
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

fn sessions_from(bundle: &SessionBundle, secrets: &[Change]) -> String {
    let mut authors: Vec<String> = Vec::new();
    for (index, session) in bundle.sessions.iter().enumerate() {
        let author = shown(index, &Field::OriginAuthor, author_of(session), secrets);
        if !authors.contains(&author) {
            authors.push(author);
        }
    }
    let count = noun(bundle.sessions.len(), "session", "sessions");
    if authors.is_empty() {
        count
    } else {
        format!("{count} from {}", authors.join(", "))
    }
}

fn listed(index: usize, session: &BundledSession, secrets: &[Change]) -> String {
    let origin = &session.origin;
    let dates = match (origin.created_at, origin.updated_at) {
        (Some(created), Some(updated)) => format!(
            "{} to {}",
            created.format("%Y-%m-%d"),
            updated.format("%Y-%m-%d")
        ),
        (Some(_) | None, None) | (None, Some(_)) => "undated".to_owned(),
    };
    format!(
        "{} ({}): {} from {}, {dates}\n",
        shown(
            index,
            &Field::OriginSessionKey,
            &origin.session_key,
            secrets
        ),
        shown(index, &Field::OriginAgent, &origin.agent, secrets),
        noun(session.batch.messages.len(), "message", "messages"),
        shown(index, &Field::OriginAuthor, author_of(session), secrets),
    )
}

fn shown(index: usize, field: &Field, text: &str, secrets: &[Change]) -> String {
    match secrets
        .iter()
        .find(|change| change.session == Some(index) && change.field == *field)
    {
        Some(change) => change.replacement.clone(),
        None => text.to_owned(),
    }
}

fn author_of(session: &BundledSession) -> &str {
    session.origin.known_author().unwrap_or("an unknown author")
}

fn kernel_batch(batch: wire::capture::CaptureBatchRequest) -> kernel::memory::CaptureBatchRequest {
    kernel::memory::CaptureBatchRequest {
        session_key: batch.session_key,
        agent: batch.agent,
        channel: batch.channel,
        title: batch.title,
        settings: batch.settings,
        task_id: batch.task_id,
        finalize: batch.finalize,
        created_at: batch.created_at,
        updated_at: batch.updated_at,
        messages: batch
            .messages
            .into_iter()
            .map(|message| kernel::memory::CaptureMessage {
                event_id: message.event_id,
                role: message.role,
                content: message.content,
                tool_name: message.tool_name,
                tool_args: message.tool_args,
                tool_result: message.tool_result,
                metadata: message.metadata,
                created_at: message.created_at,
            })
            .collect(),
    }
}
