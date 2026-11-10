use std::fmt::Write as _;

use rmcp::model::CallToolResult;

use super::Export;
use super::Sanitize;
use super::Sessions;
use super::files::Kind;
use super::noun;
use super::refuse_existing;
use crate::Error;
use kernel::memory::share::DecideMode;
use kernel::memory::share::FlagSummary;
use kernel::memory::share::SanitizeJob;
use kernel::memory::share::SanitizeSummary;

const FLAGS_SHOWN: usize = 50;

pub(crate) async fn export(kit: &Sessions, args: Export) -> Result<CallToolResult, Error> {
    let path = kit.files.target(&args.out).await?;
    refuse_existing(&[&path]).await?;
    let count = kit
        .share
        .export(args.conversation_ids.as_slice(), path.clone())
        .await?;
    Ok(crate::text(format!(
        "bundled {} (raw, not sanitized) to {}\nsanitize it with session_sanitize before it leaves this machine\n",
        noun(count, "session", "sessions"),
        path.display()
    )))
}

pub(crate) async fn sanitize(kit: &Sessions, args: Sanitize) -> Result<CallToolResult, Error> {
    let input = kit.files.readable(&args.input, Kind::Bundle).await?;
    let mapping = match &args.mapping {
        Some(source) => Some(kit.files.readable(source, Kind::Mapping).await?),
        None => None,
    };
    let out = kit.files.target(&args.out).await?;
    let summary = kit
        .share
        .sanitize(SanitizeJob {
            input,
            out,
            mapping,
            decide: args.decide == Some(true),
        })
        .await?;
    Ok(crate::text(sanitized(&summary)?))
}

fn sanitized(summary: &SanitizeSummary) -> Result<String, Error> {
    let mut out = String::new();
    writeln!(
        out,
        "sanitized {}: {}",
        noun(summary.sessions, "session", "sessions"),
        summary.totals
    )?;
    writeln!(out, "bundle: {}", summary.bundle.display())?;
    writeln!(out, "report: {}", summary.report.display())?;
    if let Some(local) = &summary.local {
        writeln!(out, "local review file: {}", local.display())?;
    }
    writeln!(out, "changes by rule: {}", summary.rule_counts)?;
    writeln!(out, "{}", summary.not_scanned)?;
    writeln!(out, "{}", summary.best_effort)?;
    if let Some(decision) = &summary.decision {
        let mode = match decision.mode {
            DecideMode::Advisory => "advisory: ingest does not wait",
            DecideMode::Block => "block: import refuses the bundle until every flag is marked",
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
        let mut flags: Vec<&FlagSummary> = decision.flags.iter().collect();
        flags.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.number.cmp(&right.number))
        });
        for flag in flags.iter().take(FLAGS_SHOWN) {
            writeln!(out, "{}", flag_line(flag))?;
        }
        if flags.len() > FLAGS_SHOWN {
            writeln!(out, "and {} more in the report", flags.len() - FLAGS_SHOWN)?;
        }
    }
    writeln!(
        out,
        "share only the bundle, never the report or the local review file; mark flags on their Mark lines in the report and run `rh memory sanitize --resolve {}`",
        summary.report.display()
    )?;
    Ok(out)
}

fn flag_line(flag: &FlagSummary) -> String {
    let mut parts = vec![
        format!("flag {}", flag.number),
        format!("score {:.2}", flag.score),
    ];
    if let Some(place) = &flag.place {
        let message = match place.message {
            Some(message) => format!(" message {}", message + 1),
            None => String::new(),
        };
        let also = if place.more == 0 {
            String::new()
        } else {
            format!(" (and {})", noun(place.more, "more place", "more places"))
        };
        parts.push(format!(
            "session {}{message} {}{also}",
            place.session + 1,
            place.field
        ));
    }
    if let Some(mark) = flag.report_line {
        parts.push(format!("report line {mark}"));
    }
    parts.push(noun(flag.chars, "char", "chars"));
    parts.join("  ")
}
