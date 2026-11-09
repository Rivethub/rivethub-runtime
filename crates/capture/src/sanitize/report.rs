use std::fmt;

use chrono::DateTime;
use chrono::Utc;
use wire::capture::BundledSession;

use super::mapping::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub path: String,
    pub key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Title,
    Settings(Spot),
    Content,
    ToolArgs(Spot),
    ToolResult,
    Metadata(Spot),
    OriginNode,
    ExporterNode,
}

impl fmt::Display for Field {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (root, spot) = match self {
            Self::Title => return formatter.write_str("title"),
            Self::Content => return formatter.write_str("content"),
            Self::ToolResult => return formatter.write_str("tool_result"),
            Self::OriginNode => return formatter.write_str("origin.node"),
            Self::ExporterNode => return formatter.write_str("manifest.exported_by.node"),
            Self::Settings(spot) => ("settings", spot),
            Self::ToolArgs(spot) => ("tool_args", spot),
            Self::Metadata(spot) => ("metadata", spot),
        };
        formatter.write_str(root)?;
        if !spot.path.is_empty() {
            write!(formatter, ".{}", spot.path)?;
        }
        if spot.key {
            formatter.write_str(" (a key)")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumIter)]
#[strum(serialize_all = "snake_case")]
pub enum Pass {
    Secrets,
    Mapping,
    Hosts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    Pattern(&'static str),
    SecretKey,
    Mapped { line: usize },
    Redacted { line: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Secret,
    Mapped,
    Redacted,
}

impl RuleId {
    #[must_use]
    pub const fn change_kind(self) -> ChangeKind {
        match self {
            Self::Pattern(_) | Self::SecretKey => ChangeKind::Secret,
            Self::Mapped { .. } => ChangeKind::Mapped,
            Self::Redacted { .. } => ChangeKind::Redacted,
        }
    }

    const fn order(self) -> usize {
        match self {
            Self::Pattern(_) | Self::SecretKey => 0,
            Self::Mapped { line } | Self::Redacted { line } => line,
        }
    }

    const fn pass(self) -> Pass {
        match self {
            Self::Pattern(_) | Self::SecretKey => Pass::Secrets,
            Self::Mapped { .. } | Self::Redacted { .. } => Pass::Mapping,
        }
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pattern(name) => formatter.write_str(name),
            Self::SecretKey => formatter.write_str("secret key name"),
            Self::Mapped { line } => write!(formatter, "rule at line {line} (map)"),
            Self::Redacted { line } => write!(formatter, "rule at line {line} (redact)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub session: Option<usize>,
    pub event_id: Option<String>,
    pub field: Field,
    pub rule: RuleId,
    pub fingerprint: String,
    pub replacement: String,
}

impl fmt::Display for Change {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.session {
            Some(index) => write!(formatter, "session {}", index + 1)?,
            None => formatter.write_str("manifest")?,
        }
        if let Some(event_id) = &self.event_id {
            write!(formatter, " event {event_id}")?;
        }
        write!(formatter, " {}: {}", self.field, self.rule)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLine {
    pub conversation_id: String,
    pub agent: String,
    pub messages: usize,
}

impl SessionLine {
    pub(crate) fn of(session: &BundledSession) -> Self {
        Self {
            conversation_id: session.origin.conversation_id.clone(),
            agent: session.origin.agent.clone(),
            messages: session.batch.messages.len(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub at: DateTime<Utc>,
    pub policy_sha256: String,
    pub bundle_sha256: String,
    pub sessions: Vec<SessionLine>,
    pub rules: Vec<(usize, Action)>,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub secrets: usize,
    pub mapped: usize,
    pub redacted: usize,
}

impl Report {
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for change in &self.changes {
            match change.rule.change_kind() {
                ChangeKind::Secret => totals.secrets += 1,
                ChangeKind::Mapped => totals.mapped += 1,
                ChangeKind::Redacted => totals.redacted += 1,
            }
        }
        totals
    }

    #[must_use]
    pub fn markdown(&self, bundle: &str, mapping: &str) -> String {
        let totals = self.totals();
        let mut out = String::new();
        let sessions = self.sessions.len();
        push_line(
            &mut out,
            format_args!("# Sanitize report for {}\n", cell(bundle)),
        );
        push_line(
            &mut out,
            format_args!(
                "Sanitized {sessions} {} at {}: {} secrets, {} mapped, {} redacted.\n",
                plural(sessions, "session", "sessions"),
                self.at.format("%Y-%m-%d %H:%M:%S UTC"),
                totals.secrets,
                totals.mapped,
                totals.redacted,
            ),
        );
        out.push_str(
            "This report never shows the text that was replaced. The Original column is a short hash of the replaced text, keyed by the raw bundle, so equal values share a hash and nobody without the raw bundle can test a guess against it.\n\n",
        );
        push_line(&mut out, format_args!("- Mapping: {}", cell(mapping)));
        push_line(
            &mut out,
            format_args!("- Policy sha256: `{}`", self.policy_sha256),
        );
        push_line(
            &mut out,
            format_args!("- Bundle sha256: `{}`\n", self.bundle_sha256),
        );
        self.rule_table(&mut out);
        for (index, session) in self.sessions.iter().enumerate() {
            push_line(
                &mut out,
                format_args!(
                    "## Session {}: {} ({}, {} {})\n",
                    index + 1,
                    cell(&session.conversation_id),
                    cell(&session.agent),
                    session.messages,
                    plural(session.messages, "message", "messages"),
                ),
            );
            change_table(
                &mut out,
                self.changes.iter().filter(|c| c.session == Some(index)),
            );
        }
        if self.changes.iter().any(|change| change.session.is_none()) {
            out.push_str("## Manifest\n\n");
            change_table(
                &mut out,
                self.changes.iter().filter(|c| c.session.is_none()),
            );
        }
        out
    }

    fn rule_table(&self, out: &mut String) {
        out.push_str(
            "## Changes per rule\n\n| Pass | Rule | Replaced with | Changes |\n|---|---|---|---|\n",
        );
        let mut rows: Vec<(RuleId, String, usize)> = Vec::new();
        for change in &self.changes {
            match rows.iter_mut().find(|row| row.0 == change.rule) {
                Some(row) => row.2 += 1,
                None => rows.push((change.rule, change.replacement.clone(), 1)),
            }
        }
        for (line, action) in &self.rules {
            let rule = match action {
                Action::Map(_) => RuleId::Mapped { line: *line },
                Action::Redact => RuleId::Redacted { line: *line },
            };
            if !rows.iter().any(|row| row.0 == rule) {
                rows.push((rule, action.replacement().to_owned(), 0));
            }
        }
        rows.sort_by_key(|row| row.0.order());
        if rows.is_empty() {
            out.push_str("| - | no rule matched | - | 0 |\n");
        }
        for (rule, replacement, count) in rows {
            push_line(
                out,
                format_args!(
                    "| {} | {} | {} | {count} |",
                    rule.pass(),
                    rule,
                    code(&replacement)
                ),
            );
        }
        out.push('\n');
    }
}

struct Row<'a> {
    event_id: &'a str,
    field: String,
    rule: RuleId,
    replacement: &'a str,
    count: usize,
    fingerprints: Vec<&'a str>,
}

const SHOWN_FINGERPRINTS: usize = 3;

fn change_table<'a>(out: &mut String, changes: impl Iterator<Item = &'a Change>) {
    let mut rows: Vec<Row<'a>> = Vec::new();
    for change in changes {
        let event_id = change.event_id.as_deref().unwrap_or("-");
        let field = change.field.to_string();
        let found = rows
            .iter_mut()
            .find(|row| row.event_id == event_id && row.field == field && row.rule == change.rule);
        match found {
            Some(row) => {
                row.count += 1;
                if !row.fingerprints.contains(&change.fingerprint.as_str()) {
                    row.fingerprints.push(&change.fingerprint);
                }
            }
            None => rows.push(Row {
                event_id,
                field,
                rule: change.rule,
                replacement: &change.replacement,
                count: 1,
                fingerprints: vec![&change.fingerprint],
            }),
        }
    }
    if rows.is_empty() {
        out.push_str("No changes.\n\n");
        return;
    }
    out.push_str(
        "| Event | Field | Rule | Replaced with | Count | Original |\n|---|---|---|---|---|---|\n",
    );
    for row in rows {
        let mut shown: Vec<String> = row
            .fingerprints
            .iter()
            .take(SHOWN_FINGERPRINTS)
            .map(|fingerprint| format!("`{fingerprint}`"))
            .collect();
        if row.fingerprints.len() > SHOWN_FINGERPRINTS {
            shown.push(format!(
                "and {} more",
                row.fingerprints.len() - SHOWN_FINGERPRINTS
            ));
        }
        push_line(
            out,
            format_args!(
                "| {} | {} | {} | {} | {} | {} |",
                cell(row.event_id),
                cell(&row.field),
                row.rule,
                code(row.replacement),
                row.count,
                shown.join(", ")
            ),
        );
    }
    out.push('\n');
}

fn cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

fn code(text: &str) -> String {
    format!("`{}`", cell(&text.replace('`', "'")))
}

const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn push_line(out: &mut String, line: fmt::Arguments<'_>) {
    out.push_str(&line.to_string());
    out.push('\n');
}
