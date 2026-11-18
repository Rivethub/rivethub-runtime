use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::filing::CorrectionExample;
use crate::filing::Subject;
use crate::filing::Target;
use crate::memory::ConversationHead;
use crate::topics::PlacementSource;
use crate::topics::Root;
use crate::topics::RootKind;
use crate::topics::TopicError;
use crate::topics::TopicId;
use crate::topics::TopicKind;
use crate::topics::TopicName;

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde_as(as = "values::time::Rfc3339")]
    pub taken_at: Timestamp,
    pub conversations: Vec<SnapshotConversation>,
    pub topics: Vec<SnapshotTopic>,
    pub redirects: Vec<SnapshotRedirect>,
    pub placements: Vec<SnapshotPlacement>,
    pub corrections: Vec<SnapshotCorrection>,
    pub projects: Vec<Project>,
    pub refused: Vec<String>,
    pub pages: Vec<KnowledgePage>,
    pub sections: Vec<EntitySection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotHead {
    pub id: Uuid,
    pub session_key: String,
    pub agent: String,
    pub channel: String,
    pub title: Option<String>,
}

impl SnapshotHead {
    #[must_use]
    pub fn head(&self) -> ConversationHead {
        ConversationHead {
            id: self.id,
            session_key: self.session_key.clone(),
            agent: self.agent.clone(),
            channel: self.channel.clone(),
            title: self.title.clone(),
        }
    }
}

impl From<ConversationHead> for SnapshotHead {
    fn from(head: ConversationHead) -> Self {
        Self {
            id: head.id,
            session_key: head.session_key,
            agent: head.agent,
            channel: head.channel,
            title: head.title,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum TaskOrigin {
    Heartbeat,
    Chat,
    Tool,
    Mesh,
    Api,
    Eval,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotConversation {
    #[serde(flatten)]
    pub head: SnapshotHead,
    pub cwd: Option<String>,
    pub summary: Option<String>,

    #[serde_as(as = "values::time::Rfc3339")]
    pub last_active: Timestamp,
    pub task_parent: Option<SnapshotHead>,
    pub task_origin: Option<TaskOrigin>,
    pub opening: Option<String>,
    pub user_turns: u32,
    pub parent_session: Option<String>,
}

impl SnapshotConversation {
    #[must_use]
    pub fn subject(&self) -> Subject {
        Subject {
            head: self.head.head(),
            cwd: self.cwd.clone(),
            summary: self.summary.clone(),
        }
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.head.title.as_deref()
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTopic {
    pub id: TopicId,
    pub kind: TopicKind,
    pub name: String,
    pub parent: Option<TopicId>,
    pub aliases: Vec<String>,
    pub directory: Option<SnapshotRoot>,

    #[serde_as(as = "values::time::Rfc3339")]
    pub created_at: Timestamp,
    #[serde_as(as = "values::time::Rfc3339")]
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRoot {
    pub shared: bool,
    pub node: Option<String>,
    pub path: String,
}

impl SnapshotRoot {
    #[must_use]
    pub fn of(root: &Root) -> Self {
        Self {
            shared: root.kind() == RootKind::Shared,
            node: root.node().map(str::to_owned),
            path: root.path().to_owned(),
        }
    }

    #[must_use]
    pub fn root(&self) -> Option<Root> {
        let kind = if self.shared {
            RootKind::Shared
        } else {
            RootKind::Local
        };
        Root::from_parts(kind, self.node.clone(), self.path.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRedirect {
    pub slug: String,
    pub topic: TopicId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPlacement {
    pub session: Option<String>,
    pub conversation: Option<Uuid>,
    pub topic: TopicId,
    pub by: PlacementSource,
}

impl SnapshotPlacement {
    #[must_use]
    pub fn target(&self) -> Option<Target> {
        Target::stored(self.session.as_deref(), self.conversation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCorrection {
    pub session: Option<String>,
    pub conversation: Option<Uuid>,
    pub from: String,
    pub to: Option<String>,
    pub title: Option<String>,
}

impl SnapshotCorrection {
    #[must_use]
    pub fn target(&self) -> Option<Target> {
        Target::stored(self.session.as_deref(), self.conversation)
    }

    pub fn example(&self) -> Result<CorrectionExample, TopicError> {
        Ok(CorrectionExample {
            title: self.title.clone(),
            from: TopicName::new(&self.from)?,
            to: self.to.as_deref().map(TopicName::new).transpose()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgePage {
    pub slug: String,
    pub title: String,
    pub lead: String,
    pub entities: Vec<String>,
    pub cited: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySection {
    pub page: String,
    pub heading: String,
    pub topic: TopicId,
    pub lead: String,
}
