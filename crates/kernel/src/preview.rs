use crate::transcript::HarnessTranscriptTurn;
use crate::transcript::SessionMessageRole;
use chrono::DateTime;
use chrono::Utc;

pub const PREVIEW_CHARS: usize = 200;
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

#[cfg(test)]
mod tests {
    use super::*;

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
