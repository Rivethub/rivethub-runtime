use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use thiserror::Error;

use super::decide::Decision;
use super::decide::Flag;
use super::decide::Mode;
use super::decide::Resolution;
use super::report::cell;
use super::report::code;
use super::report::plural;
use super::report::push_line;

pub(super) const STAMP_PREFIX: &str = "- Stamp: ";
const OPEN_PREFIX: &str = "Unreviewed: ";
const FLAG_HEADING: &str = "### Flag ";
const HIDDEN_OPEN: &str = "<!-- flag ";
const HIDDEN_CLOSE: &str = " -->";
const SHORTEST_LEARNED: usize = 3;

pub(super) fn stamp_text(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

pub(super) fn flag_section(out: &mut String, decision: &Decision, bundle: &str) {
    let count = decision.flags.len();
    let review = decision.review;
    out.push_str("## Flags to review\n\n");
    push_line(
        out,
        format_args!(
            "After the secrets and mapping passes, the decision model {} scored {} {} in {} {}. It skipped {} trivial {}: identifiers (UUIDs, hex digests, base64url ids, times, and metadata id and path fields), placeholders, `[tool call] <name>` lines, and bare tokens under 16 characters that hold none of `@ / = , : ;`, no capitalized word, no non-ASCII letter, no mix of letters and digits, and no IP, phone or host-name shape, such as numbers and enum values. Below are the chunks it scored highest, up to {} per session, at or over {:.2}. Each flag covers every place in the bundle, in any session and inside other flagged text, JSON keys, tool names, channels and task ids, where its text occurs outside a placeholder.\n",
            code(&decision.model),
            decision.scored,
            plural(decision.scored, "chunk", "chunks"),
            decision.calls,
            plural(decision.calls, "call", "calls"),
            decision.trivial,
            plural(decision.trivial, "chunk", "chunks"),
            review.top,
            review.threshold,
        ),
    );
    match review.mode {
        Mode::Advisory => push_line(
            out,
            format_args!(
                "This review is advisory. {} holds each flagged chunk as sanitized text, and ingest does not wait for you. The bundle's stamp records which flags you have marked.\n",
                cell(bundle),
            ),
        ),
        Mode::Block => push_line(
            out,
            format_args!(
                "This review blocks. Until you mark a flag, {} holds `[FLAGGED:<n>]` in place of its text at every place, so that text leaves this machine only if you keep it. Ingest refuses the bundle while any flag is open.\n",
                cell(bundle),
            ),
        ),
    }
    push_line(out, format_args!("{OPEN_PREFIX}{count} of {count}\n"));
    if count == 0 {
        return;
    }
    let keep = match review.mode {
        Mode::Advisory => "leaves the text as it is and records that you reviewed it",
        Mode::Block => "puts the text back as shown",
    };
    push_line(
        out,
        format_args!(
            "To review a flag, replace `open` on its Mark line with one of these marks, then run `rh memory sanitize --resolve <this report>`:\n\n\
             - `keep` {keep}.\n\
             - `redact` replaces the text with `[REDACTED:flagged]`.\n\
             - `map <term> as <placeholder>` replaces each `<term>` in the text with `<placeholder>`; a term that starts or ends with a letter, digit or underscore matches whole words only. The placeholder may not be a `[FLAGGED:<n>]` or `[REDACTED...]` form or look like a secret. Add `--learn` to also add the rule to your mapping file (terms of 3 or more characters).\n\n\
             Do not edit the text under a flag: `--resolve` refuses a flag whose text changed. You can mark some flags now and the rest later; a flag whose section you delete stays open.\n"
        ),
    );
    for flag in &decision.flags {
        flag_entry(out, flag);
    }
}

fn flag_entry(out: &mut String, flag: &Flag) {
    let fence = "~".repeat(longest_run(&flag.text, '~').max(2) + 1);
    let mut sessions: Vec<&str> = Vec::new();
    for shown in &flag.shown {
        if !sessions.contains(&shown.session.as_str()) {
            sessions.push(&shown.session);
        }
    }
    let ids: Vec<String> = flag
        .shown
        .iter()
        .map(|shown| code(&format!("{}:{}", shown.owner, shown.field)))
        .collect();
    let (first, more) = ids
        .split_first()
        .map_or(("`-`", &[][..]), |(first, more)| (first.as_str(), more));
    let also = if more.is_empty() {
        String::new()
    } else {
        format!("\n- Also at: {}", more.join(", "))
    };
    push_line(
        out,
        format_args!(
            "{FLAG_HEADING}{} (score {:.2})\n\n- Session: {}\n- Id: {first}{also}\n\nMark: open\n\n{fence}text\n{}\n{fence}\n\n{HIDDEN_OPEN}{}: {}{HIDDEN_CLOSE}\n",
            flag.number,
            flag.score,
            sessions
                .iter()
                .map(|session| code(session))
                .collect::<Vec<_>>()
                .join(", "),
            flag.text,
            flag.number,
            BASE64.encode(&flag.text),
        ),
    );
}

fn longest_run(text: &str, wanted: char) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == wanted { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkState {
    Open,
    Marked(Resolution),
    Resolved(Resolution),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFlag {
    pub number: usize,
    pub text: String,
    pub shown: String,
    pub state: MarkState,
    line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFlags {
    pub at: DateTime<Utc>,
    pub flags: Vec<ReportFlag>,
    open_line: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReportError {
    #[error("this is not a sanitize report: it has no `{}` line", STAMP_PREFIX.trim())]
    NoStamp,
    #[error("the report stamp `{0}` is not a time")]
    BadStamp(String),
    #[error("the report has no flags section; it was written without --decide")]
    NoFlags,
    #[error("flag {number} appears twice in the report")]
    Twice { number: usize },
    #[error("flag {number} has no Mark line")]
    NoMark { number: usize },
    #[error("flag {number} lost its hidden copy; put the section back as sanitize wrote it")]
    NoOriginal { number: usize },
    #[error("flag {number}: {problem}")]
    Mark { number: usize, problem: MarkProblem },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MarkProblem {
    #[error("`{0}` is not a mark; write keep, redact, or map <term> as <placeholder>")]
    Unknown(String),
    #[error("write a map mark as `map <term> as <placeholder>`")]
    MapShape,
    #[error("the map term is blank")]
    BlankTerm,
    #[error("the flagged text does not contain `{0}` as a whole word")]
    TermMissing(String),
    #[error("the map term `{0}` cuts into a placeholder in the flagged text")]
    CutsPlaceholder(String),
    #[error(
        "the placeholder `{0}` holds a `[FLAGGED:<n>]` or `[REDACTED...]` form; pick a plain placeholder"
    )]
    PlaceholderForm(String),
    #[error("the placeholder looks like a secret to the secrets scan; pick a plain placeholder")]
    SecretForm,
    #[error(
        "`{0}` is shorter than {SHORTEST_LEARNED} characters, too short to learn as a mapping rule"
    )]
    TooShort(String),
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keep => formatter.write_str("keep"),
            Self::Redact => formatter.write_str("redact"),
            Self::Map { term, placeholder } => {
                write!(formatter, "map `{term}` as `{placeholder}`")
            }
        }
    }
}

#[derive(Default)]
struct Pending {
    number: usize,
    mark: Option<(usize, String, bool)>,
    hidden: Option<String>,
    shown: Option<Vec<String>>,
}

pub fn read_flags(report: &str) -> Result<ReportFlags, ReportError> {
    let mut at = None;
    let mut open_line = None;
    let mut pending: Vec<Pending> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in report.split('\n').enumerate() {
        let trimmed = line.trim();
        if let Some((wanted, length)) = fence {
            if trimmed.chars().all(|c| c == wanted) && trimmed.chars().count() >= length {
                fence = None;
            } else if let Some(lines) = pending.last_mut().and_then(|flag| flag.shown.as_mut()) {
                lines.push(line.to_owned());
            }
            continue;
        }
        if let Some(opened) = ['~', '`']
            .into_iter()
            .find(|&c| trimmed.starts_with(&c.to_string().repeat(3)))
        {
            fence = Some((opened, trimmed.chars().take_while(|&c| c == opened).count()));
            if let Some(current) = pending.last_mut()
                && current.shown.is_none()
            {
                current.shown = Some(Vec::new());
            }
            continue;
        }
        if let Some(stamp) = line.strip_prefix(STAMP_PREFIX) {
            let stamp = stamp.trim().trim_matches('`');
            at = Some(
                DateTime::parse_from_rfc3339(stamp)
                    .map_err(|_| ReportError::BadStamp(stamp.to_owned()))?
                    .with_timezone(&Utc),
            );
        } else if line.starts_with(OPEN_PREFIX) {
            open_line = Some(index);
        } else if let Some(rest) = line.strip_prefix(FLAG_HEADING) {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(number) = digits.parse() {
                if pending.iter().any(|flag| flag.number == number) {
                    return Err(ReportError::Twice { number });
                }
                pending.push(Pending {
                    number,
                    ..Pending::default()
                });
            }
        } else if let Some(current) = pending.last_mut() {
            if let Some(encoded) = line
                .strip_prefix(HIDDEN_OPEN)
                .and_then(|rest| rest.strip_suffix(HIDDEN_CLOSE))
                .and_then(|rest| rest.split_once(": "))
                .filter(|(number, _)| number.parse() == Ok(current.number))
                .map(|(_, encoded)| encoded)
            {
                current.hidden = Some(encoded.to_owned());
            } else if current.mark.is_none() {
                current.mark = mark_line(trimmed).map(|(value, resolved)| (index, value, resolved));
            }
        }
    }
    let at = at.ok_or(ReportError::NoStamp)?;
    let open_line = open_line.ok_or(ReportError::NoFlags)?;
    let flags = pending
        .into_iter()
        .map(ReportFlag::read)
        .collect::<Result<_, _>>()?;
    Ok(ReportFlags {
        at,
        flags,
        open_line,
    })
}

impl ReportFlag {
    fn read(pending: Pending) -> Result<Self, ReportError> {
        let number = pending.number;
        let (line, value, resolved) = pending.mark.ok_or(ReportError::NoMark { number })?;
        let lost = || ReportError::NoOriginal { number };
        let encoded = pending.hidden.ok_or_else(lost)?;
        let bytes = BASE64.decode(encoded.trim()).map_err(|_| lost())?;
        let text = String::from_utf8(bytes).map_err(|_| lost())?;
        let state = match (resolution(&value, &text), resolved) {
            (Ok(None), false) => MarkState::Open,
            (Ok(Some(resolution)), false) => MarkState::Marked(resolution),
            (Ok(Some(resolution)), true) => MarkState::Resolved(resolution),
            (Ok(None), true) => {
                return Err(ReportError::Mark {
                    number,
                    problem: MarkProblem::Unknown(value),
                });
            }
            (Err(problem), _) => return Err(ReportError::Mark { number, problem }),
        };
        Ok(Self {
            number,
            shown: pending.shown.unwrap_or_default().join("\n"),
            text,
            state,
            line,
        })
    }

    #[must_use]
    pub fn shows_its_text(&self) -> bool {
        let lines = |text: &str| -> Vec<String> {
            text.lines()
                .map(|line| line.trim_end().to_owned())
                .collect()
        };
        lines(&self.shown) == lines(&self.text)
    }

    pub fn learnable(&self) -> Result<(), ReportError> {
        match &self.state {
            MarkState::Marked(Resolution::Map { term, .. })
            | MarkState::Resolved(Resolution::Map { term, .. })
                if term.chars().filter(|c| !c.is_whitespace()).count() < SHORTEST_LEARNED =>
            {
                Err(ReportError::Mark {
                    number: self.number,
                    problem: MarkProblem::TooShort(term.clone()),
                })
            }
            MarkState::Open | MarkState::Marked(_) | MarkState::Resolved(_) => Ok(()),
        }
    }
}

fn emphasis(text: &str) -> &str {
    let length = text
        .chars()
        .take_while(|c| matches!(c, '*' | '_'))
        .take(2)
        .count();
    text.get(..length).unwrap_or_default()
}

fn unwrapped(text: &str) -> &str {
    let mut text = text.trim();
    loop {
        let marker = emphasis(text);
        match text
            .strip_prefix(marker)
            .and_then(|rest| rest.strip_suffix(marker))
            .filter(|inner| !marker.is_empty() && !inner.trim().is_empty())
        {
            Some(inner) => text = inner.trim(),
            None => return text,
        }
    }
}

fn mark_line(line: &str) -> Option<(String, bool)> {
    let bare = unwrapped(line.trim_start_matches(['-', '>', ' ']));
    let marker = emphasis(bare);
    let labelled = bare.get(marker.len()..).unwrap_or_default();
    for (label, resolved) in [("mark:", false), ("resolved:", true)] {
        if let Some(head) = labelled.get(..label.len())
            && head.eq_ignore_ascii_case(label)
        {
            let rest = labelled.get(label.len()..).unwrap_or_default();
            let rest = rest.strip_prefix(marker).unwrap_or(rest);
            return Some((unwrapped(rest).to_owned(), resolved));
        }
    }
    None
}

#[derive(strum::EnumString)]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
enum MarkWord {
    Open,
    Keep,
    Redact,
    Map,
}

fn resolution(value: &str, original: &str) -> Result<Option<Resolution>, MarkProblem> {
    let value = value.trim();
    let value = value
        .strip_prefix('`')
        .and_then(|inner| inner.strip_suffix('`'))
        .unwrap_or(value)
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    let (head, rest) = match value.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (value, ""),
    };
    let unknown = || MarkProblem::Unknown(value.to_owned());
    let word: MarkWord = head.parse().map_err(|_| unknown())?;
    match (word, rest.is_empty()) {
        (MarkWord::Open, true) => Ok(None),
        (MarkWord::Keep, true) => Ok(Some(Resolution::Keep)),
        (MarkWord::Redact, true) => Ok(Some(Resolution::Redact)),
        (MarkWord::Map, false) => map_mark(rest, original).map(Some),
        (MarkWord::Map, true) => Err(MarkProblem::MapShape),
        (MarkWord::Open | MarkWord::Keep | MarkWord::Redact, false) => Err(unknown()),
    }
}

fn map_mark(rest: &str, original: &str) -> Result<Resolution, MarkProblem> {
    let (term, placeholder) = rest.rsplit_once(" as ").ok_or(MarkProblem::MapShape)?;
    let term = unquote(term);
    let placeholder = unquote(placeholder);
    if term.trim().is_empty() {
        return Err(MarkProblem::BlankTerm);
    }
    if placeholder.trim().is_empty() {
        return Err(MarkProblem::MapShape);
    }
    let spans = super::decide::term_spans(original, term);
    if spans.is_empty() {
        return Err(MarkProblem::TermMissing(term.to_owned()));
    }
    let protected = super::scrub::protected(original, crate::rules::ruleset());
    if spans.iter().any(|span| {
        protected
            .iter()
            .any(|held| span.start < held.end && held.start < span.end)
    }) {
        return Err(MarkProblem::CutsPlaceholder(term.to_owned()));
    }
    if [crate::rules::names::FLAGGED_OPEN, "[REDACTED"]
        .iter()
        .any(|form| placeholder.contains(form))
    {
        return Err(MarkProblem::PlaceholderForm(placeholder.to_owned()));
    }
    if super::scrub::scan_flags(placeholder) {
        return Err(MarkProblem::SecretForm);
    }
    Ok(Resolution::Map {
        term: term.to_owned(),
        placeholder: placeholder.to_owned(),
    })
}

fn unquote(text: &str) -> &str {
    let text = text.trim();
    ['"', '\'', '`']
        .into_iter()
        .find_map(|quote| {
            text.strip_prefix(quote)
                .and_then(|inner| inner.strip_suffix(quote))
        })
        .unwrap_or(text)
}

impl ReportFlags {
    #[must_use]
    pub fn rewrite(&self, report: &str, open: usize, total: usize, applied: &[usize]) -> String {
        let mut lines: Vec<String> = report.split('\n').map(str::to_owned).collect();
        if let Some(line) = lines.get_mut(self.open_line) {
            *line = format!("{OPEN_PREFIX}{open} of {total}");
        }
        for flag in &self.flags {
            if let (MarkState::Marked(resolution), Some(line)) =
                (&flag.state, lines.get_mut(flag.line))
                && applied.contains(&flag.number)
            {
                *line = format!("Resolved: {resolution}");
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emphasis_wraps_the_label_or_the_whole_mark_and_never_the_term() {
        for (line, value) in [
            ("Mark: keep", "keep"),
            ("**Mark:** keep", "keep"),
            ("**Mark: keep**", "keep"),
            ("_Mark:_ *redact*", "redact"),
            (
                "- Mark: map `pg_prod_07` as DB_HOST",
                "map `pg_prod_07` as DB_HOST",
            ),
            ("Mark: map a*b*c as __X__", "map a*b*c as __X__"),
            ("Resolved: map `x` as `y`", "map `x` as `y`"),
        ] {
            assert_eq!(
                mark_line(line).map(|(value, _)| value),
                Some(value.to_owned()),
                "{line}"
            );
        }
        assert_eq!(mark_line("Marked by Phil"), None);
    }

    #[test]
    fn a_map_mark_keeps_the_characters_of_its_term() {
        assert_eq!(
            resolution("map `halyard_app` as DB_USER", "use halyard_app now"),
            Ok(Some(Resolution::Map {
                term: "halyard_app".to_owned(),
                placeholder: "DB_USER".to_owned(),
            }))
        );
    }
}
