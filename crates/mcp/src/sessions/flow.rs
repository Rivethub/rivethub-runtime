use std::fmt::Write as _;
use std::path::Path;

use capture::sanitize::decide::Flag;
use capture::sanitize::decide::Mode;
use capture::share;
use capture::share::BundlePaths;
use capture::share::Clobber;
use capture::share::Outcome;
use capture::share::SetupError;
use chrono::Utc;
use config::MappingFile;
use rmcp::model::CallToolResult;
use wire::capture::BundledSession;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::Export;
use super::Sanitize;
use super::Sessions;
use super::files::Kind;
use super::files::refuse_existing;
use super::noun;
use crate::Error;

const FLAGS_SHOWN: usize = 50;

pub(crate) async fn export(kit: &Sessions, args: Export) -> Result<CallToolResult, Error> {
    let path = kit.files.target(&args.out).await?;
    refuse_existing(&[&path]).await?;
    let wanted = args.conversation_ids.as_slice();
    let sessions = kit.archive.export(wanted).await?;
    let missing: Vec<String> = wanted
        .iter()
        .map(ToString::to_string)
        .filter(|id| {
            !sessions
                .iter()
                .any(|session| session.origin.conversation_id.eq_ignore_ascii_case(id))
        })
        .collect();
    if !missing.is_empty() {
        return Err(Error::UnknownSessions(missing.join(", ")));
    }
    let sessions: Vec<BundledSession> = sessions
        .into_iter()
        .map(BundledSession::forwarding_stored_origin)
        .collect();
    let count = sessions.len();
    let mut text =
        capture::bundle::seal(kit.author.clone(), Utc::now(), Sanitization::Raw, sessions)?.text;
    text.push(b'\n');
    share::write_file(path.clone(), text, Clobber::Refuse).await?;
    Ok(crate::text(format!(
        "bundled {} (raw, not sanitized) to {}\nsanitize it with session_sanitize before it leaves this machine\n",
        noun(count, "session", "sessions"),
        path.display()
    )))
}

pub(crate) async fn sanitize(kit: &Sessions, args: Sanitize) -> Result<CallToolResult, Error> {
    let decider = match (args.decide == Some(true), &kit.decider) {
        (false, _) => None,
        (true, Ok(decider)) => Some(decider),
        (true, Err(SetupError::Unconfigured)) => return Err(Error::DecideUnconfigured),
        (true, Err(error)) => return Err(share::ShareError::from(error.clone()).into()),
    };
    let input = kit.files.readable(&args.input, Kind::Bundle).await?;
    let mapping = match &args.mapping {
        Some(source) => MappingFile::Configured(kit.files.readable(source, Kind::Mapping).await?),
        None => kit.mapping.clone(),
    };
    let paths = BundlePaths::beside(&kit.files.target(&args.out).await?);
    refuse_existing(&[&paths.bundle, &paths.report, &paths.local]).await?;
    let bundle = read_bundle(&input).await?;
    let outcome = share::sanitize(share::Job {
        bundle: &bundle,
        mapping: &mapping,
        key: &kit.key,
        decider,
        paths: &paths,
        clobber: Clobber::Refuse,
        at: Utc::now(),
    })
    .await?;
    Ok(crate::text(sanitized(&outcome, &paths)?))
}

pub(crate) async fn read_bundle(path: &Path) -> Result<SessionBundle, Error> {
    let not_bundle = || Error::NotBundle(path.to_path_buf());
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| not_bundle())?;
    capture::bundle::parse(&text).map_err(|_| not_bundle())
}

fn sanitized(outcome: &Outcome, paths: &BundlePaths) -> Result<String, Error> {
    let report = &outcome.report;
    let mut out = String::new();
    writeln!(
        out,
        "sanitized {}: {}",
        noun(outcome.sanitized.sessions.len(), "session", "sessions"),
        report.totals()
    )?;
    writeln!(out, "bundle: {}", paths.bundle.display())?;
    writeln!(out, "report: {}", paths.report.display())?;
    if outcome.local {
        writeln!(out, "local review file: {}", paths.local.display())?;
    }
    writeln!(out, "changes by rule: {}", share::rule_counts(report))?;
    writeln!(out, "{}", share::NOT_SCANNED)?;
    writeln!(out, "{}", share::BEST_EFFORT)?;
    if let Some(decision) = &report.decision {
        let mode = match decision.review.mode {
            Mode::Advisory => "advisory: ingest does not wait",
            Mode::Block => "block: import refuses the bundle until every flag is marked",
        };
        writeln!(
            out,
            "decision pass: {} scored {} in {}, {} skipped; {} open ({mode})",
            decision.model,
            noun(decision.scored, "chunk", "chunks"),
            noun(decision.calls, "call", "calls"),
            noun(decision.trivial, "trivial chunk", "trivial chunks"),
            noun(decision.flags.len(), "flag", "flags"),
        )?;
        let mut flags: Vec<&Flag> = decision.flags.iter().collect();
        flags.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.number.cmp(&right.number))
        });
        for flag in flags.iter().take(FLAGS_SHOWN) {
            writeln!(out, "{}", flag_line(flag, outcome))?;
        }
        if flags.len() > FLAGS_SHOWN {
            writeln!(out, "and {} more in the report", flags.len() - FLAGS_SHOWN)?;
        }
    }
    writeln!(
        out,
        "share only the bundle, never the report or the local review file; mark flags on their Mark lines in the report and run `rh memory sanitize --resolve {}`",
        paths.report.display()
    )?;
    Ok(out)
}

fn flag_line(flag: &Flag, outcome: &Outcome) -> String {
    let mut parts = vec![
        format!("flag {}", flag.number),
        format!("score {:.2}", flag.score),
    ];
    if let Some((place, more)) = flag.places().split_first() {
        let message = match place.field.message {
            Some(message) => format!(" message {}", message + 1),
            None => String::new(),
        };
        let also = if more.is_empty() {
            String::new()
        } else {
            format!(" (and {})", noun(more.len(), "more place", "more places"))
        };
        parts.push(format!(
            "session {}{message} {}{also}",
            place.field.session + 1,
            place.field.name
        ));
    }
    if let Some(mark) = outcome.mark_lines.get(&flag.number) {
        parts.push(format!("report line {mark}"));
    }
    parts.push(noun(flag.text.chars().count(), "char", "chars"));
    parts.join("  ")
}
