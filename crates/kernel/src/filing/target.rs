use std::str::FromStr;

use strum::IntoEnumIterator;
use uuid::Uuid;
use values::HarnessId;
use values::SessionId;

use crate::memory::ConversationHead;

const SANDBOX_RUN_PREFIX: &str = "grokbot-run-sand-subagent-";
const HEARTBEAT_PREFIX: &str = "heartbeat";
const TASK_PREFIX: &str = "task:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString, strum::Display, strum::EnumIter)]
#[strum(serialize_all = "kebab-case")]
enum UnfiledChannel {
    TestChannel,
    EnvTestChannel,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Session(SessionId),
    Conversation(Uuid),
}

impl Target {
    #[must_use]
    pub fn session(&self) -> Option<&SessionId> {
        match self {
            Self::Session(session) => Some(session),
            Self::Conversation(_) => None,
        }
    }

    #[must_use]
    pub fn conversation(&self) -> Option<Uuid> {
        match self {
            Self::Conversation(conversation) => Some(*conversation),
            Self::Session(_) => None,
        }
    }

    #[must_use]
    pub fn stored(session: Option<&str>, conversation: Option<Uuid>) -> Option<Self> {
        match (session, conversation) {
            (Some(session), None) => {
                #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
                let Ok(session) = SessionId::from_str(session) else {
                    return None;
                };
                Some(Self::Session(session))
            }
            (None, Some(conversation)) => Some(Self::Conversation(conversation)),
            (Some(_), Some(_)) | (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filable {
    Session(SessionId),
    Task(Uuid),
    Conversation,
    Never,
}

impl Filable {
    #[must_use]
    pub fn of(head: &ConversationHead) -> Self {
        let key = head.session_key.as_str();
        if key.starts_with(SANDBOX_RUN_PREFIX)
            || key.starts_with(HEARTBEAT_PREFIX)
            || UnfiledChannel::from_str(&head.channel).is_ok()
        {
            return Self::Never;
        }
        if let Some(task) = key.strip_prefix(TASK_PREFIX) {
            #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
            let Ok(task) = Uuid::parse_str(task) else {
                return Self::Conversation;
            };
            return Self::Task(task);
        }
        if bare_claude_session(head) {
            return Self::Session(SessionId::new(HarnessId::ClaudeCode, key));
        }
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(session) = SessionId::from_str(key) else {
            return Self::Conversation;
        };
        Self::Session(session)
    }

    #[must_use]
    pub fn target(&self, conversation: Uuid) -> Option<Target> {
        match self {
            Self::Session(session) => Some(Target::Session(session.clone())),
            Self::Task(_) | Self::Conversation => Some(Target::Conversation(conversation)),
            Self::Never => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub key_prefixes: Vec<String>,
    pub channels: Vec<String>,
}

impl LeftOut {
    #[must_use]
    pub fn rules() -> Self {
        Self {
            key_prefixes: vec![SANDBOX_RUN_PREFIX.to_owned(), HEARTBEAT_PREFIX.to_owned()],
            channels: UnfiledChannel::iter()
                .map(|channel| channel.to_string())
                .collect(),
        }
    }
}

#[must_use]
pub fn bare_session_prefix() -> String {
    format!("{}:", HarnessId::ClaudeCode)
}

#[must_use]
pub fn conversation_keys(session: &SessionId) -> Vec<String> {
    let mut keys = vec![session.as_str().to_owned()];
    if session.harness() == HarnessId::ClaudeCode && Uuid::parse_str(session.native()).is_ok() {
        keys.push(session.native().to_owned());
    }
    keys
}

#[must_use]
pub fn session_key(channel: Option<&str>, key: &str) -> String {
    if bare_claude(channel, key) {
        SessionId::new(HarnessId::ClaudeCode, key)
            .as_str()
            .to_owned()
    } else {
        key.to_owned()
    }
}

#[must_use]
pub fn stored_keys(session: &str) -> Vec<String> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(id) = SessionId::from_str(session) else {
        return vec![session.to_owned()];
    };
    conversation_keys(&id)
}

fn bare_claude_session(head: &ConversationHead) -> bool {
    bare_claude(Some(&head.channel), &head.session_key)
}

fn bare_claude(channel: Option<&str>, key: &str) -> bool {
    channel.is_some_and(|channel| {
        HarnessId::from_str(channel).is_ok_and(|harness| harness == HarnessId::ClaudeCode)
    }) && Uuid::parse_str(key).is_ok()
}
