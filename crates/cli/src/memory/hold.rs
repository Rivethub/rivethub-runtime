use capture::receive::Checked;
use capture::receive::Decisions;
use capture::receive::Plan;
use wire::capture::SessionBundle;

use super::share::ShareError;
use crate::Error;
use crate::Exit;
use crate::Report;
use crate::argv::Scan;
use crate::argv::Taken;

pub(super) fn take_decision(scan: &mut Scan<'_>, decisions: &mut Decisions) -> Result<bool, Error> {
    match scan.option("--session", None) {
        Taken::Value(value) => {
            let (number, decision) = value.split_once('=').ok_or(ShareError::SessionDecision)?;
            let number: usize = number.parse().map_err(|_| ShareError::SessionDecision)?;
            let index = number.checked_sub(1).ok_or(ShareError::SessionDecision)?;
            decisions
                .sessions
                .insert(index, decision.parse().map_err(ShareError::from)?);
            return Ok(true);
        }
        Taken::Missing => return Err(ShareError::SessionDecision.into()),
        Taken::Absent => {}
    }
    match scan.option("--all", None) {
        Taken::Value(value) => {
            decisions.all = Some(value.parse().map_err(ShareError::from)?);
            Ok(true)
        }
        Taken::Missing => Err(ShareError::SessionDecision.into()),
        Taken::Absent => Ok(false),
    }
}

pub(super) fn held_report(checked: &Checked, bundle: &SessionBundle, rerun: &str) -> Report {
    let flagged = checked.flagged();
    let total = bundle.sessions.len();
    let what = if total == 1 { "session" } else { "sessions" };
    let has = if flagged.len() == 1 { "has" } else { "have" };
    let numbers: Vec<String> = flagged
        .iter()
        .map(|index| (index + 1).to_string())
        .collect();
    let decide = match numbers.as_slice() {
        [one] => format!("--session {one}=import|skip|hold-spans"),
        many => format!(
            "--session <n>=import|skip|hold-spans for each of {}, or --all=skip|hold-spans",
            many.join(", ")
        ),
    };
    let mut stdout = format!(
        "held: {} of {total} {what} {has} findings ({}); nothing imported. Decide with {decide}\n",
        flagged.len(),
        checked.summary()
    );
    if checked
        .findings()
        .iter()
        .any(|finding| finding.family == capture::receive::Family::Secret)
    {
        stdout.push_str(
            "A session with secret findings can only be skipped or have its spans held, unless you pass --force.\n",
        );
    }
    stdout.push_str(rerun);
    stdout.push_str("\n\n");
    stdout.push_str(&checked.report(bundle));
    Report {
        stdout,
        stderr: String::new(),
        exit: Exit::Held,
        stdout_raw: Vec::new(),
    }
}

pub(super) fn decided(plan: &Plan<'_>) -> String {
    let held: Vec<String> = plan
        .imports
        .iter()
        .filter(|accepted| accepted.checks.decision == wire::capture::ChecksDecision::HoldSpans)
        .map(|accepted| (accepted.index + 1).to_string())
        .collect();
    let skipped: Vec<String> = plan
        .skipped
        .iter()
        .map(|index| (index + 1).to_string())
        .collect();
    let mut notes = Vec::new();
    if !held.is_empty() {
        notes.push(format!("spans held in {}", held.join(", ")));
    }
    if !skipped.is_empty() {
        notes.push(format!("skipped {}", skipped.join(", ")));
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join("; "))
    }
}
