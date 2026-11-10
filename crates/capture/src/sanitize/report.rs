use std::collections::HashMap;
use std::fmt;

use chrono::DateTime;
use chrono::Utc;
use wire::capture::BundledSession;

use super::decide::Decision;
use super::mapping::Action;
use super::marks::STAMP_PREFIX;
use super::marks::flag_section;
use super::marks::stamp_text;

const SHOWN_FINGERPRINTS: usize = 3;
const SHOWN_EVENT_HEX: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Spot {
    pub path: String,
    pub key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Field {
    Title,
    Settings(Spot),
    Content,
    ToolArgs(Spot),
    ToolResult,
    ToolName,
    Metadata(Spot),
    EventId,
    SessionKey,
    Agent,
    Channel,
    TaskId,
    OriginAuthor,
    OriginNode,
    OriginConversation,
    OriginSessionKey,
    OriginAgent,
    ExporterUser,
    ExporterNode,
}

impl fmt::Display for Field {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (root, spot) = match self {
            Self::Title => return formatter.write_str("title"),
            Self::Content => return formatter.write_str("content"),
            Self::ToolResult => return formatter.write_str("tool_result"),
            Self::ToolName => return formatter.write_str("tool_name"),
            Self::EventId => return formatter.write_str("event_id"),
            Self::SessionKey => return formatter.write_str("session_key"),
            Self::Agent => return formatter.write_str("agent"),
            Self::Channel => return formatter.write_str("channel"),
            Self::TaskId => return formatter.write_str("task_id"),
            Self::OriginAuthor => return formatter.write_str("origin.author"),
            Self::OriginNode => return formatter.write_str("origin.node"),
            Self::OriginConversation => return formatter.write_str("origin.conversation_id"),
            Self::OriginSessionKey => return formatter.write_str("origin.session_key"),
            Self::OriginAgent => return formatter.write_str("origin.agent"),
            Self::ExporterUser => return formatter.write_str("manifest.exported_by.user"),
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
    Decide,
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
    pub message: Option<usize>,
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
        if let Some(position) = self.message {
            write!(formatter, " message {}", position + 1)?;
        }
        write!(formatter, " {}: {}", self.field, self.rule)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub session: Option<usize>,
    pub message: Option<usize>,
    pub event_id: Option<String>,
    pub field: Field,
    pub key: String,
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

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub at: DateTime<Utc>,
    pub policy_sha256: String,
    pub bundle_sha256: String,
    pub sessions: Vec<SessionLine>,
    pub rules: Vec<(usize, Action)>,
    pub changes: Vec<Change>,
    pub collisions: Vec<Collision>,
    pub rechecked: usize,
    pub decision: Option<Decision>,
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
        let mut out = String::new();
        self.header(&mut out, bundle, mapping);
        if let Some(decision) = &self.decision {
            flag_section(&mut out, decision, bundle);
        }
        self.rule_table(&mut out);
        let mut by_session: HashMap<Option<usize>, Vec<&Change>> = HashMap::new();
        for change in &self.changes {
            by_session.entry(change.session).or_default().push(change);
        }
        let mut collisions: HashMap<Option<usize>, Vec<&Collision>> = HashMap::new();
        for collision in &self.collisions {
            collisions
                .entry(collision.session)
                .or_default()
                .push(collision);
        }
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
                by_session.get(&Some(index)).map_or(&[], Vec::as_slice),
            );
            collision_lines(
                &mut out,
                collisions.get(&Some(index)).map_or(&[], Vec::as_slice),
            );
        }
        if let Some(changes) = by_session.get(&None) {
            out.push_str("## Manifest\n\n");
            change_table(&mut out, changes);
        }
        out
    }

    fn header(&self, out: &mut String, bundle: &str, mapping: &str) {
        let totals = self.totals();
        let sessions = self.sessions.len();
        push_line(
            out,
            format_args!("# Sanitize report for {}\n", cell(bundle)),
        );
        out.push_str(
            "The sanitizer is best-effort. It reliably catches known vendor tokens, assignments to password- and token-named keys in env, JSON, YAML, XML, SQL and code, name/value pairs with such names, URL credentials, PEM and DER private keys, base64 blobs that decode to any of those, and mapped terms. It can miss unusual code forms, vendor shapes it has no rule for, and passwords written as bare words in prose or with spaces outside quotes. Read this report before the bundle leaves the machine.\n\n",
        );
        push_line(
            out,
            format_args!(
                "Sanitized {sessions} {} at {}: {} secrets, {} mapped, {} redacted.\n",
                plural(sessions, "session", "sessions"),
                self.at.format("%Y-%m-%d %H:%M:%S UTC"),
                totals.secrets,
                totals.mapped,
                totals.redacted,
            ),
        );
        if self.rechecked > 0 {
            push_line(
                out,
                format_args!(
                    "The re-check that ingest runs found {} more {} after the first pass; they are redacted below. Each one is a sanitizer bug worth reporting.\n",
                    self.rechecked,
                    plural(self.rechecked, "span", "spans"),
                ),
            );
        }
        out.push_str(
            "This report never shows the text that was replaced. The Original column is a short HMAC of each replaced span under this machine's local sanitize key and the raw bundle's digest, so two spans with the same text share a fingerprint and nobody without the key can test a guess against it.\n\n",
        );
        out.push_str(
            "Every text field is scanned for secrets, the author included; the author and task id are not mapped. Dates and roles are not scanned. Event ids are replaced by an HMAC of the raw id under the local key.\n\n",
        );
        let rules = crate::rules::ruleset();
        let (ours, theirs) = rules.counts();
        push_line(
            out,
            format_args!(
                "- Secret rules: {ours} RivetHub rules and {theirs} gitleaks rules ({})",
                rules.upstream().version
            ),
        );
        push_line(out, format_args!("- Mapping: {}", cell(mapping)));
        push_line(
            out,
            format_args!("- Policy sha256: `{}`", self.policy_sha256),
        );
        push_line(
            out,
            format_args!("- Bundle sha256: `{}`", self.bundle_sha256),
        );
        push_line(
            out,
            format_args!("{STAMP_PREFIX}`{}`\n", stamp_text(self.at)),
        );
    }

    fn rule_table(&self, out: &mut String) {
        out.push_str(
            "## Changes per rule\n\n| Pass | Rule | Replaced with | Changes |\n|---|---|---|---|\n",
        );
        let mut rows: Vec<(RuleId, String, usize)> = Vec::new();
        let mut index: HashMap<RuleId, usize> = HashMap::new();
        for change in &self.changes {
            if let Some(&at) = index.get(&change.rule) {
                if let Some(row) = rows.get_mut(at) {
                    row.2 += 1;
                }
            } else {
                index.insert(change.rule, rows.len());
                rows.push((change.rule, change.replacement.clone(), 1));
            }
        }
        for (line, action) in &self.rules {
            let rule = match action {
                Action::Map(_) => RuleId::Mapped { line: *line },
                Action::Redact => RuleId::Redacted { line: *line },
            };
            if let std::collections::hash_map::Entry::Vacant(slot) = index.entry(rule) {
                slot.insert(rows.len());
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

fn change_table(out: &mut String, changes: &[&Change]) {
    let mut rows: Vec<Row<'_>> = Vec::new();
    let mut index: HashMap<(&str, &Field, RuleId), usize> = HashMap::new();
    for change in changes {
        let event_id = change.event_id.as_deref().unwrap_or("-");
        if let Some(&at) = index.get(&(event_id, &change.field, change.rule)) {
            if let Some(row) = rows.get_mut(at) {
                row.count += 1;
                if !row.fingerprints.contains(&change.fingerprint.as_str()) {
                    row.fingerprints.push(&change.fingerprint);
                }
            }
        } else {
            index.insert((event_id, &change.field, change.rule), rows.len());
            rows.push(Row {
                event_id,
                field: change.field.to_string(),
                rule: change.rule,
                replacement: &change.replacement,
                count: 1,
                fingerprints: vec![&change.fingerprint],
            });
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
                cell(short_event(row.event_id)),
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

fn collision_lines(out: &mut String, collisions: &[&Collision]) {
    if collisions.is_empty() {
        return;
    }
    out.push_str("Keys renamed because two keys scrubbed to the same text:\n\n");
    for collision in collisions {
        push_line(
            out,
            format_args!(
                "- event {}, {}: {}",
                cell(short_event(collision.event_id.as_deref().unwrap_or("-"))),
                cell(&collision.field.to_string()),
                code(&collision.key)
            ),
        );
    }
    out.push('\n');
}

fn short_event(event_id: &str) -> &str {
    event_id.get(..SHOWN_EVENT_HEX).unwrap_or(event_id)
}

pub(super) fn cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

pub(super) fn code(text: &str) -> String {
    format!("`{}`", cell(&text.replace('`', "'")))
}

pub(super) const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

pub(super) fn push_line(out: &mut String, line: fmt::Arguments<'_>) {
    out.push_str(&line.to_string());
    out.push('\n');
}
