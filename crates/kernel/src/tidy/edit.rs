use std::collections::BTreeSet;
use std::str::FromStr;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use uuid::Uuid;
use values::SessionId;

use super::Fingerprint;
use super::TidyError;
use crate::filing::Target;
use crate::topics::Alias;
use crate::topics::Slug;
use crate::topics::TopicId;
use crate::topics::TopicName;
use crate::wiki::page::WikiSlug;

const ROOT: &str = "root";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEdit {
    Merge {
        source: TopicId,
        into: TopicId,
    },
    Move {
        topic: TopicId,
        parent: Option<TopicId>,
    },
    Rename {
        topic: TopicId,
        name: TopicName,
    },
    Close {
        topic: TopicId,
    },
    Delete {
        topic: TopicId,
    },
    CreateTopic {
        topic: TopicId,
        name: TopicName,
        parent: Option<TopicId>,
        aliases: Vec<Alias>,
        members: Vec<Target>,
    },
    FileInto {
        topic: TopicId,
        members: Vec<Target>,
    },
    MergePage {
        page: WikiSlug,
        into: WikiSlug,
    },
    RemovePage {
        page: WikiSlug,
    },
    AddType {
        topic: TopicId,
        name: TopicName,
        parent: Option<TopicId>,
    },
}

impl TreeEdit {
    pub fn fingerprint(&self) -> Result<Fingerprint, TidyError> {
        Ok(match self {
            Self::Merge { source, into } => {
                let (low, high) = if source < into {
                    (source, into)
                } else {
                    (into, source)
                };
                Fingerprint::of(&["merge", &low.to_string(), &high.to_string()])
            }
            Self::Move { topic, parent } => Fingerprint::of(&[
                "move",
                &topic.to_string(),
                &parent.map_or_else(|| ROOT.to_owned(), |parent| parent.to_string()),
            ]),
            Self::Rename { topic, name } => {
                Fingerprint::of(&["rename", &topic.to_string(), slug(name)?.as_str()])
            }
            Self::Close { topic } => Fingerprint::of(&["close", &topic.to_string()]),
            Self::Delete { topic } => Fingerprint::of(&["delete", &topic.to_string()]),
            Self::CreateTopic { name, .. } => Fingerprint::of(&["create", slug(name)?.as_str()]),
            Self::FileInto { topic, members } => {
                Fingerprint::of(&["file", &topic.to_string(), &member_digest(members)])
            }
            Self::MergePage { page, into } => {
                Fingerprint::of(&["merge-page", page.as_ref(), into.as_ref()])
            }
            Self::RemovePage { page } => Fingerprint::of(&["remove-page", page.as_ref()]),
            Self::AddType { name, .. } => Fingerprint::of(&["type", slug(name)?.as_str()]),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EditRecord {
    Merge {
        source: String,
        into: String,
    },
    Move {
        topic: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    Rename {
        topic: String,
        name: String,
    },
    Close {
        topic: String,
    },
    Delete {
        topic: String,
    },
    CreateTopic {
        topic: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        aliases: Vec<String>,
        #[serde(default)]
        sessions: Vec<SessionId>,
        #[serde(default)]
        conversations: Vec<String>,
    },
    FileInto {
        topic: String,
        #[serde(default)]
        sessions: Vec<SessionId>,
        #[serde(default)]
        conversations: Vec<String>,
    },
    MergePage {
        page: String,
        into: String,
    },
    RemovePage {
        page: String,
    },
    AddType {
        topic: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
}

impl From<&TreeEdit> for EditRecord {
    fn from(edit: &TreeEdit) -> Self {
        let id = |topic: &TopicId| topic.to_string();
        let parent = |parent: &Option<TopicId>| parent.as_ref().map(id);
        match edit {
            TreeEdit::Merge { source, into } => Self::Merge {
                source: id(source),
                into: id(into),
            },
            TreeEdit::Move { topic, parent: to } => Self::Move {
                topic: id(topic),
                parent: parent(to),
            },
            TreeEdit::Rename { topic, name } => Self::Rename {
                topic: id(topic),
                name: name.as_str().to_owned(),
            },
            TreeEdit::Close { topic } => Self::Close { topic: id(topic) },
            TreeEdit::Delete { topic } => Self::Delete { topic: id(topic) },
            TreeEdit::CreateTopic {
                topic,
                name,
                parent: under,
                aliases,
                members,
            } => Self::CreateTopic {
                topic: id(topic),
                name: name.as_str().to_owned(),
                parent: parent(under),
                aliases: aliases
                    .iter()
                    .map(|alias| alias.as_str().to_owned())
                    .collect(),
                sessions: sessions(members),
                conversations: conversations(members),
            },
            TreeEdit::FileInto { topic, members } => Self::FileInto {
                topic: id(topic),
                sessions: sessions(members),
                conversations: conversations(members),
            },
            TreeEdit::MergePage { page, into } => Self::MergePage {
                page: page.as_ref().to_owned(),
                into: into.as_ref().to_owned(),
            },
            TreeEdit::RemovePage { page } => Self::RemovePage {
                page: page.as_ref().to_owned(),
            },
            TreeEdit::AddType {
                topic,
                name,
                parent: under,
            } => Self::AddType {
                topic: id(topic),
                name: name.as_str().to_owned(),
                parent: parent(under),
            },
        }
    }
}

impl TryFrom<EditRecord> for TreeEdit {
    type Error = TidyError;

    fn try_from(change: EditRecord) -> Result<Self, TidyError> {
        Ok(match change {
            EditRecord::Merge { source, into } => Self::Merge {
                source: topic(&source)?,
                into: topic(&into)?,
            },
            EditRecord::Move {
                topic: moved,
                parent,
            } => Self::Move {
                topic: topic(&moved)?,
                parent: parent.as_deref().map(topic).transpose()?,
            },
            EditRecord::Rename { topic: id, name } => Self::Rename {
                topic: topic(&id)?,
                name: TopicName::new(&name)?,
            },
            EditRecord::Close { topic: id } => Self::Close { topic: topic(&id)? },
            EditRecord::Delete { topic: id } => Self::Delete { topic: topic(&id)? },
            EditRecord::CreateTopic {
                topic: id,
                name,
                parent,
                aliases,
                sessions,
                conversations,
            } => Self::CreateTopic {
                topic: topic(&id)?,
                name: TopicName::new(&name)?,
                parent: parent.as_deref().map(topic).transpose()?,
                aliases: aliases
                    .iter()
                    .map(|alias| Alias::new(alias))
                    .collect::<Result<_, _>>()?,
                members: members(sessions, &conversations)?,
            },
            EditRecord::FileInto {
                topic: id,
                sessions,
                conversations,
            } => Self::FileInto {
                topic: topic(&id)?,
                members: members(sessions, &conversations)?,
            },
            EditRecord::MergePage { page, into } => Self::MergePage {
                page: wiki_slug(&page)?,
                into: wiki_slug(&into)?,
            },
            EditRecord::RemovePage { page } => Self::RemovePage {
                page: wiki_slug(&page)?,
            },
            EditRecord::AddType {
                topic: id,
                name,
                parent,
            } => Self::AddType {
                topic: topic(&id)?,
                name: TopicName::new(&name)?,
                parent: parent.as_deref().map(topic).transpose()?,
            },
        })
    }
}

fn member_digest(members: &[Target]) -> String {
    let ids: BTreeSet<String> = members
        .iter()
        .map(|member| match member {
            Target::Session(session) => session.as_str().to_owned(),
            Target::Conversation(conversation) => conversation.to_string(),
        })
        .collect();
    let joined: Vec<&str> = ids.iter().map(String::as_str).collect();
    hex::encode(Sha256::digest(joined.join("\n").as_bytes()))
}

fn sessions(members: &[Target]) -> Vec<SessionId> {
    members
        .iter()
        .filter_map(Target::session)
        .cloned()
        .collect()
}

fn conversations(members: &[Target]) -> Vec<String> {
    members
        .iter()
        .filter_map(Target::conversation)
        .map(|conversation| conversation.to_string())
        .collect()
}

fn members(sessions: Vec<SessionId>, conversations: &[String]) -> Result<Vec<Target>, TidyError> {
    sessions
        .into_iter()
        .map(Target::Session)
        .map(Ok)
        .chain(conversations.iter().map(|text| {
            Uuid::parse_str(text)
                .map(Target::Conversation)
                .map_err(|_| invalid(text))
        }))
        .collect()
}

fn wiki_slug(text: &str) -> Result<WikiSlug, TidyError> {
    WikiSlug::try_from(text.to_owned()).map_err(|_| invalid(text))
}

fn slug(name: &TopicName) -> Result<Slug, TidyError> {
    Ok(Slug::fold(name.as_str())?)
}

fn topic(text: &str) -> Result<TopicId, TidyError> {
    TopicId::from_str(text).map_err(|_| invalid(text))
}

fn invalid(text: &str) -> TidyError {
    TidyError::Invalid {
        what: text.to_owned(),
    }
}
