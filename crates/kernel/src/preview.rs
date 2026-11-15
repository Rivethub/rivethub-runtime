use crate::transcript::HarnessTranscriptTurn;
use crate::transcript::SessionMessageRole;
use chrono::DateTime;
use chrono::Utc;
use strum::IntoEnumIterator as _;
use values::JsonObject;

pub const PREVIEW_CHARS: usize = 200;
const WRITING_CHARS: usize = 600;
#[derive(Clone, Copy, strum::EnumIter, strum::Display)]
#[strum(serialize_all = "snake_case")]
enum Gist {
    Command,
    FilePath,
    Path,
    Pattern,
    Query,
    Url,
    Description,
    Prompt,
}

#[derive(strum::EnumString)]
enum PromptTool {
    #[strum(serialize = "askuserquestion")]
    Joined,
    #[strum(serialize = "ask_user_question")]
    Snake,
    #[strum(serialize = "ask_user")]
    Short,
}

pub fn is_prompt_tool(name: &str) -> bool {
    name.trim().to_lowercase().parse::<PromptTool>().is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Running,
    Asking,
    Approval,
    Said,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub kind: PreviewKind,
    pub text: String,
    pub at: DateTime<Utc>,
}

impl Preview {
    pub fn new(kind: PreviewKind, text: &str, at: DateTime<Utc>) -> Option<Self> {
        one_line(text).map(|text| Self { kind, text, at })
    }

    pub fn said(reply: &str, at: DateTime<Utc>) -> Option<Self> {
        Self::new(PreviewKind::Said, last_line(reply)?, at)
    }

    pub fn from_transcript(
        turns: &[HarnessTranscriptTurn],
        updated: DateTime<Utc>,
    ) -> Option<Self> {
        let reply = turns.iter().rev().find(|turn| {
            turn.role == SessionMessageRole::Assistant && last_line(&turn.text).is_some()
        })?;
        let at = reply
            .ended_at_ms
            .or(reply.started_at_ms)
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or(updated);
        Self::said(&reply.text, at)
    }
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().rev().find(|line| !line.trim().is_empty())
}

fn one_line(text: &str) -> Option<String> {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return None;
    }
    if joined.chars().count() <= PREVIEW_CHARS {
        return Some(joined);
    }
    let mut clipped: String = joined.chars().take(PREVIEW_CHARS - 1).collect();
    clipped.push('…');
    Some(clipped)
}

fn gist(input: &JsonObject) -> Option<&str> {
    Gist::iter()
        .find_map(|key| input.get(&key.to_string()).and_then(|value| value.as_str()))
        .map(str::trim)
        .filter(|gist| !gist.is_empty())
}

pub fn tool_label(name: &str, input: &JsonObject) -> String {
    match gist(input).filter(|gist| !name.contains(gist)) {
        Some(gist) => format!("{name}: {gist}"),
        None => name.to_owned(),
    }
}

fn question(input: &JsonObject) -> Option<&str> {
    input
        .get("questions")
        .and_then(|value| value.as_array())
        .and_then(|questions| {
            questions
                .iter()
                .find_map(|asked| asked.get("question").and_then(|value| value.as_str()))
        })
        .or_else(|| input.get("question").and_then(|value| value.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Doing {
    id: String,
    name: String,
    label: String,
    at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveActivity {
    tools: Vec<Doing>,
    asking: Option<Doing>,
    approval: Option<Doing>,
    writing: String,
    wrote_at: Option<DateTime<Utc>>,
}

impl LiveActivity {
    pub fn wrote(&mut self, text: &str, at: DateTime<Utc>) {
        self.writing.push_str(text);
        let excess = self.writing.chars().count().saturating_sub(WRITING_CHARS);
        if let Some((cut, _)) = self
            .writing
            .char_indices()
            .nth(excess)
            .filter(|_| excess > 0)
        {
            self.writing.drain(..cut);
        }
        self.wrote_at = Some(at);
    }

    pub fn tool_started(&mut self, call: &str, name: &str, input: &JsonObject, at: DateTime<Utc>) {
        self.writing.clear();
        let asked = question(input).filter(|_| is_prompt_tool(name));
        let doing = Doing {
            id: call.to_owned(),
            name: name.to_owned(),
            label: asked.map_or_else(|| tool_label(name, input), str::to_owned),
            at,
        };
        match asked {
            Some(_) => self.asking = Some(doing),
            None => self.tools.push(doing),
        }
    }

    pub fn tool_updated(&mut self, call: &str, title: Option<&str>, input: Option<&JsonObject>) {
        let Some(doing) = self.tools.iter_mut().find(|doing| doing.id == call) else {
            return;
        };
        if let Some(gist) = input.and_then(gist) {
            doing.label = format!("{}: {gist}", doing.name);
            return;
        }
        if let Some(title) = title
            .map(|title| title.trim().trim_matches('`').trim())
            .filter(|title| !title.is_empty())
        {
            title.clone_into(&mut doing.label);
        }
    }

    #[must_use]
    pub fn running(&self, call: &str) -> bool {
        self.tools.iter().any(|doing| doing.id == call)
            || self.asking.as_ref().is_some_and(|asking| asking.id == call)
    }

    pub fn tool_finished(&mut self, call: &str) {
        self.tools.retain(|doing| doing.id != call);
        if self.asking.as_ref().is_some_and(|asking| asking.id == call) {
            self.asking = None;
        }
    }

    pub fn approval_requested(
        &mut self,
        request: &str,
        name: &str,
        input: &JsonObject,
        at: DateTime<Utc>,
    ) {
        self.approval = Some(Doing {
            id: request.to_owned(),
            name: name.to_owned(),
            label: tool_label(name, input),
            at,
        });
    }

    pub fn approval_resolved(&mut self, request: &str) {
        if self
            .approval
            .as_ref()
            .is_some_and(|approval| approval.id == request)
        {
            self.approval = None;
        }
    }

    pub fn preview(&self) -> Option<Preview> {
        if let Some(approval) = &self.approval {
            return Preview::new(PreviewKind::Approval, &approval.label, approval.at);
        }
        if let Some(asking) = &self.asking {
            return Preview::new(PreviewKind::Asking, &asking.label, asking.at);
        }
        if let Some(tool) = self.tools.last() {
            return Preview::new(PreviewKind::Running, &tool.label, tool.at);
        }
        Preview::new(
            PreviewKind::Running,
            last_line(&self.writing)?,
            self.wrote_at?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(raw: &str) -> JsonObject {
        serde_json::from_str(raw).unwrap_or_default()
    }

    fn shown(activity: &LiveActivity) -> Option<(PreviewKind, String)> {
        activity
            .preview()
            .map(|preview| (preview.kind, preview.text))
    }

    #[test]
    fn a_running_tool_takes_the_command_a_later_update_names() {
        let now = Utc::now();
        let mut activity = LiveActivity::default();
        activity.tool_started("t1", "Terminal", &input("{}"), now);
        activity.tool_started("t2", "Terminal", &input("{}"), now);
        assert_eq!(tool_label("Edit", &input(r#"{"new_string":"x"}"#)), "Edit");
        assert_eq!(
            tool_label("ls -la /tmp", &input(r#"{"command":"ls -la /tmp"}"#)),
            "ls -la /tmp"
        );
        activity.tool_updated("t1", Some("`ls -la`"), None);
        activity.tool_updated(
            "t2",
            Some("`ignored`"),
            Some(&input(r#"{"command":"cargo test"}"#)),
        );
        assert_eq!(
            shown(&activity),
            Some((PreviewKind::Running, "Terminal: cargo test".into()))
        );
        activity.tool_finished("t2");
        assert_eq!(
            shown(&activity),
            Some((PreviewKind::Running, "ls -la".into()))
        );
    }

    #[test]
    fn a_live_session_shows_what_waits_on_the_person_before_what_it_is_running() {
        let now = Utc::now();
        let mut activity = LiveActivity::default();
        assert_eq!(shown(&activity), None);
        activity.wrote("Checking the build.\nRunning the ", now);
        activity.wrote("lints now", now);
        assert_eq!(
            shown(&activity),
            Some((PreviewKind::Running, "Running the lints now".into()))
        );
        activity.tool_started(
            "t1",
            "Bash",
            &input(r#"{"command":"cargo clippy --workspace"}"#),
            now,
        );
        assert_eq!(
            shown(&activity),
            Some((
                PreviewKind::Running,
                "Bash: cargo clippy --workspace".into()
            ))
        );
        activity.tool_started(
            "t2",
            "AskUserQuestion",
            &input(r#"{"questions":[{"question":"Push to main or open a PR?"}]}"#),
            now,
        );
        assert_eq!(
            shown(&activity),
            Some((PreviewKind::Asking, "Push to main or open a PR?".into()))
        );
        activity.approval_requested(
            "r1",
            "Bash",
            &input(r#"{"command":"cargo build --release"}"#),
            now,
        );
        assert_eq!(
            shown(&activity),
            Some((PreviewKind::Approval, "Bash: cargo build --release".into()))
        );
        activity.approval_resolved("r1");
        activity.tool_finished("t2");
        assert_eq!(
            shown(&activity),
            Some((
                PreviewKind::Running,
                "Bash: cargo clippy --workspace".into()
            ))
        );
        activity.tool_finished("t1");
        assert_eq!(shown(&activity), None);
    }

    #[test]
    fn an_idle_session_shows_the_last_line_of_its_last_reply_on_one_short_line() {
        let now = Utc::now();
        let turn = |role, text: &str| HarnessTranscriptTurn {
            role,
            text: text.to_owned(),
            reply_text: None,
            thinking: None,
            tools: None,
            usage: None,
            model: None,
            stop_reason: None,
            last_block: None,
            attachments: None,
            complete: true,
            compact: false,
            started_at_ms: None,
            ended_at_ms: Some(1_000),
        };
        let turns = [
            turn(SessionMessageRole::User, "hi"),
            turn(
                SessionMessageRole::Assistant,
                "Done.\n\n  All   four   checks pass.  \n",
            ),
            turn(SessionMessageRole::User, "thanks"),
            turn(SessionMessageRole::Assistant, "  "),
        ];
        let said = Preview::from_transcript(&turns, now);
        assert_eq!(
            said.map(|said| (said.kind, said.text, said.at.timestamp_millis())),
            Some((PreviewKind::Said, "All four checks pass.".into(), 1_000))
        );
        let long = "x".repeat(PREVIEW_CHARS + 50);
        let clipped = Preview::said(&long, now)
            .map(|said| said.text)
            .unwrap_or_default();
        assert_eq!(
            (clipped.chars().count(), clipped.ends_with('…')),
            (PREVIEW_CHARS, true)
        );
        assert!(is_prompt_tool(" ask_user_question ") && !is_prompt_tool("Bash"));
    }
}
