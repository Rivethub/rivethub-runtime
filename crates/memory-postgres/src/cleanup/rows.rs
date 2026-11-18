use jiff::Timestamp;
use std::str::FromStr;

use kernel::cleanup::SnapshotConversation;
use kernel::cleanup::SnapshotCorrection;
use kernel::cleanup::SnapshotHead;
use kernel::cleanup::TaskOrigin;
use uuid::Uuid;

use crate::Error;

#[derive(Debug, sqlx::FromRow)]
pub(super) struct ConversationRow {
    id: Uuid,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    cwd: Option<String>,
    last_active: DateTime<Utc>,
    opening: String,
    user_turns: i64,
    task_origin: Option<String>,
}

impl ConversationRow {
    pub(super) fn conversation(self) -> Result<SnapshotConversation, Error> {
        Ok(SnapshotConversation {
            head: SnapshotHead {
                id: self.id,
                session_key: self.session_key,
                agent: self.agent,
                channel: self.channel,
                title: self.title,
            },
            cwd: self.cwd.filter(|cwd| !cwd.trim().is_empty()),
            summary: Some(self.opening.clone()),
            last_active: self.last_active,
            task_parent: None,
            task_origin: self
                .task_origin
                .as_deref()
                .map(TaskOrigin::from_str)
                .transpose()
                .map_err(|source| Error::stored("task origin", source))?,
            opening: Some(self.opening),
            user_turns: u32::try_from(self.user_turns)?,
            parent_session: None,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct TopicRow {
    pub(super) id: Uuid,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) parent_id: Option<Uuid>,
    pub(super) directory_kind: Option<String>,
    pub(super) directory_node: Option<String>,
    pub(super) directory_path: Option<String>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    pub(super) created_at: Timestamp,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    pub(super) updated_at: Timestamp,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct AliasRow {
    pub(super) topic_id: Uuid,
    pub(super) alias: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct RedirectRow {
    pub(super) slug: String,
    pub(super) topic_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PlacementRow {
    pub(super) session_id: Option<String>,
    pub(super) conversation_id: Option<Uuid>,
    pub(super) topic_id: Uuid,
    pub(super) placed_by: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct CorrectionRow {
    session_id: Option<String>,
    conversation_id: Option<Uuid>,
    from_name: String,
    to_name: Option<String>,
    title: Option<String>,
}

impl CorrectionRow {
    pub(super) fn correction(self) -> SnapshotCorrection {
        SnapshotCorrection {
            session: self.session_id,
            conversation: self.conversation_id,
            from: self.from_name,
            to: self.to_name,
            title: self.title,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct TaxonomyRow {
    pub(super) value: String,
    pub(super) aliases: Vec<String>,
    pub(super) state: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(super) struct PageRow {
    pub(super) slug: String,
    pub(super) title: String,
    pub(super) current_state: String,
    pub(super) entities: Vec<String>,
    pub(super) cited: Vec<Uuid>,
}
