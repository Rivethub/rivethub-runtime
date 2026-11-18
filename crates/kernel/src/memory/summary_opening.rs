use serde::{Deserialize, Serialize};
use uuid::Uuid;
use values::{JsonObject, Role};

use crate::filing::Filable;
use crate::queue::{FixedJob, Job, JobKeyMode, JobName};

use super::title::opening_turn;
use super::{ConversationHead, needs_title, summary_title};

pub const OPENING_CHARS: usize = 2_000;

pub fn opening_text<'a>(ordered: impl IntoIterator<Item = (Role, &'a str)>) -> Option<String> {
    ordered.into_iter().find_map(|(role, content)| {
        if role != Role::User {
            return None;
        }
        let text = opening_turn(content)?;
        let text: String = text.chars().take(OPENING_CHARS).collect();
        substantive(text.trim_end()).then(|| text.trim_end().to_owned())
    })
}

fn substantive(text: &str) -> bool {
    let command = text
        .strip_prefix('/')
        .and_then(|tail| tail.split_whitespace().next())
        .is_some_and(|word| {
            word.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_:".contains(c))
        });
    let plain = text
        .trim_matches(|c: char| !c.is_alphanumeric())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    !command
        && !text.starts_with("Caveat:")
        && !matches!(
            plain.as_str(),
            "go ahead"
                | "keep going"
                | "please continue"
                | "continue please"
                | "thank you"
                | "thanks again"
                | "sounds good"
                | "hello there"
        )
        && text.chars().any(char::is_alphabetic)
        && text
            .split_whitespace()
            .filter(|word| word.chars().any(char::is_alphanumeric))
            .nth(1)
            .is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpeningTitle {
    pub head: ConversationHead,
    pub content: String,
    pub armed: bool,
    pub supplied: bool,
}

impl OpeningTitle {
    pub fn is_seed(&self) -> bool {
        self.armed
            && !self.supplied
            && summary_title(&self.content)
                .is_some_and(|seed| self.head.title.as_deref() == Some(seed.as_str()))
    }

    pub fn wants_model(&self) -> bool {
        !self.supplied
            && Filable::of(&self.head) != Filable::Never
            && (needs_title(self.head.title.as_deref()) || self.is_seed())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningConsumer {
    Filing,
    Title,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpeningJob {
    pub conversation_id: Uuid,
}

impl OpeningConsumer {
    pub fn job(self, conversation_id: Uuid) -> Result<Job, serde_json::Error> {
        let (task, prefix, priority, attempts) = match self {
            Self::Filing => (FixedJob::FileConversation, "file", 6, 2),
            Self::Title => (FixedJob::TitleConversation, "title", 5, 3),
        };
        Ok(Job {
            name: JobName::Fixed(task),
            payload: JsonObject::from_serialize(&OpeningJob { conversation_id })?,
            key: Some(format!("{prefix}-{conversation_id}")),
            key_mode: JobKeyMode::KeepRunAt,
            priority,
            run_at: None,
            max_attempts: Some(attempts),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_skips_non_requests_and_strips_capture_framing() {
        let request = "Repair the transfer queue";
        assert_eq!(
            opening_text([
                (Role::System, "Configure the environment"),
                (Role::Assistant, "Welcome to the session"),
                (Role::Tool, "Running the command"),
                (Role::User, "  "),
                (Role::User, "hi"),
                (Role::User, "continue"),
                (Role::User, "Please continue!"),
                (Role::User, "/model choose a different model"),
                (
                    Role::User,
                    "<command-name>/model</command-name><command-args>choose a model</command-args>"
                ),
                (
                    Role::User,
                    "<system-reminder>Run a setup check</system-reminder>"
                ),
                (
                    Role::User,
                    "# AGENTS.md instructions\n<INSTRUCTIONS>Use tabs</INSTRUCTIONS>\n<environment_context>cwd</environment_context>\n<user_query>Repair the transfer queue</user_query>"
                ),
                (Role::User, "Rewrite the transfer queue instead"),
            ]),
            Some(request.to_owned())
        );
        assert_eq!(opening_text([(Role::Assistant, request)]), None);
        assert_eq!(
            opening_text([(Role::User, "Review #421")]),
            Some("Review #421".to_owned())
        );
        assert_eq!(
            opening_text([(
                Role::User,
                "Repair the transfer queue\n<system-reminder>Ignore this frame</system-reminder>"
            )]),
            Some(request.to_owned()),
        );
    }

    #[test]
    fn opening_keeps_pasted_requests_and_caps_unicode_scalars() {
        let pasted = format!("Patch the fixture:\n{}", "é".repeat(2_100));
        let wrapped = format!("<pasted_content id=\"1\">{pasted}</pasted_content id=\"1\">");
        let expected: String = pasted.chars().take(OPENING_CHARS).collect();
        assert_eq!(
            opening_text([(Role::User, wrapped.as_str())]),
            Some(expected)
        );
        assert_eq!(
            opening_text([(
                Role::User,
                "<pasted_content id=\"1\">unrelated text</pasted_content>\nInspect the receipt"
            )]),
            Some("Inspect the receipt".to_owned())
        );
    }
}
