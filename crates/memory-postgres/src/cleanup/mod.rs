mod pages;
mod rows;

use jiff::Timestamp;
use std::str::FromStr;

use async_trait::async_trait;
use kernel::cleanup::CleanupError;
use kernel::cleanup::CleanupSource;
use kernel::cleanup::Project;
use kernel::cleanup::Snapshot;
use kernel::cleanup::SnapshotConversation;
use kernel::cleanup::SnapshotCorrection;
use kernel::cleanup::SnapshotHead;
use kernel::cleanup::SnapshotPlacement;
use kernel::cleanup::SnapshotRedirect;
use kernel::filing::Filable;
use kernel::filing::FilingStore as _;
use kernel::memory::TagState;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;

use crate::ACQUIRE_TIMEOUT;
use crate::Error;
use crate::PostgresMemory;
use rows::ConversationRow;
use rows::CorrectionRow;
use rows::PlacementRow;
use rows::RedirectRow;
use rows::TaxonomyRow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseIdentity {
    pub cluster: i64,
    pub oid: i64,
    pub name: String,
}

const READ_ONLY_SETTING: &str = "default_transaction_read_only";
const READ_ONLY_CONNECTIONS: u32 = 2;

impl PostgresMemory {
    pub async fn connect_read_only(url: &str) -> Result<Self, Error> {
        let options = PgConnectOptions::from_str(url)
            .map_err(Error::Connection)?
            .options([(READ_ONLY_SETTING, "on")]);
        let pool = crate::scoped(
            PgPoolOptions::new()
                .max_connections(READ_ONLY_CONNECTIONS)
                .acquire_timeout(ACQUIRE_TIMEOUT),
        )
        .connect_with(options)
        .await
        .map_err(Error::Connection)?;
        Ok(Self::new(pool))
    }

    pub async fn identity(&self) -> Result<DatabaseIdentity, Error> {
        let (cluster, oid, name) = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT (SELECT system_identifier FROM pg_control_system()), d.oid::bigint, d.datname::text \
             FROM pg_database d WHERE d.datname = current_database()",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|source| Error::operation("database identity", source))?;
        Ok(DatabaseIdentity { cluster, oid, name })
    }

    async fn ensure_read_only(&self) -> Result<(), Error> {
        let setting = sqlx::query_scalar::<_, String>("SELECT current_setting($1)")
            .bind(READ_ONLY_SETTING)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("read-only check", source))?;
        if setting == "on" {
            Ok(())
        } else {
            Err(Error::protocol(
                "the cleanup snapshot needs a read-only memory connection",
            ))
        }
    }

    async fn has_table(&self, table: &str) -> Result<bool, Error> {
        sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("table check", source))
    }

    async fn snapshot_conversations(&self) -> Result<Vec<SnapshotConversation>, Error> {
        let rows = sqlx::query_as::<_, ConversationRow>(
            "WITH first AS (
                 SELECT DISTINCT ON (conversation_id) conversation_id, id
                   FROM rh_summaries
                  WHERE kind = 'leaf' AND conversation_id IS NOT NULL
                  ORDER BY conversation_id, earliest_at NULLS LAST, created_at
             )
             SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                    c.settings->>'cwd' AS cwd, s.content AS summary,
                    c.updated_at AS last_active, opening.content AS opening,
                    turns.count AS user_turns, t.origin AS task_origin
               FROM first
               JOIN rh_summaries s ON s.id = first.id
               JOIN rh_conversations c ON c.id = first.conversation_id
               LEFT JOIN LATERAL (SELECT m.content FROM rh_messages m
                                   WHERE m.conversation_id = c.id AND m.role = 'user'
                                   ORDER BY m.created_at LIMIT 1) opening ON true
               LEFT JOIN LATERAL (SELECT count(*) AS count FROM rh_messages m
                                   WHERE m.conversation_id = c.id AND m.role = 'user') turns ON true
               LEFT JOIN rh_tasks_archive t ON c.session_key = 'task:' || t.id::text
              ORDER BY c.updated_at, c.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup conversations", source))?;
        let mut conversations = Vec::with_capacity(rows.len());
        for row in rows {
            let mut conversation = row.conversation()?;
            if let Filable::Task(task) = Filable::of(&conversation.head.head()) {
                conversation.task_parent = self
                    .task_parent(task, conversation.head.id)
                    .await
                    .map_err(|source| Error::stored("task parent", source))?
                    .map(SnapshotHead::from);
            }
            conversations.push(conversation);
        }
        Ok(conversations)
    }

    async fn snapshot_placements(&self) -> Result<Vec<SnapshotPlacement>, Error> {
        let mut rows = sqlx::query_as::<_, PlacementRow>(
            "SELECT session_id, NULL::uuid AS conversation_id, topic_id, placed_by
               FROM rh_session_placements",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup placements", source))?;
        if self.has_table("rh_conversation_placements").await? {
            rows.extend(
                sqlx::query_as::<_, PlacementRow>(
                    "SELECT NULL::text AS session_id, conversation_id, topic_id, placed_by
                       FROM rh_conversation_placements",
                )
                .fetch_all(&self.pool)
                .await
                .map_err(|source| Error::operation("cleanup conversation placements", source))?,
            );
        }
        rows.into_iter()
            .map(|row| {
                Ok(SnapshotPlacement {
                    session: row.session_id,
                    conversation: row.conversation_id,
                    topic: TopicId::from_uuid(row.topic_id),
                    by: PlacementSource::from_str(&row.placed_by)
                        .map_err(|error| Error::stored("placed_by", error))?,
                })
            })
            .collect()
    }

    async fn snapshot_corrections(&self) -> Result<Vec<SnapshotCorrection>, Error> {
        if !self.has_table("rh_filing_corrections").await? {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, CorrectionRow>(
            "SELECT k.session_id, k.conversation_id, f.name AS from_name, t.name AS to_name,
                    COALESCE(
                        (SELECT c.title FROM rh_conversations c WHERE c.id = k.conversation_id),
                        (SELECT c.title FROM rh_conversations c
                          WHERE c.session_key = k.session_id
                          ORDER BY c.updated_at DESC LIMIT 1)
                    ) AS title
               FROM rh_filing_corrections k
               JOIN rh_topics f ON f.id = k.from_topic
               LEFT JOIN rh_topics t ON t.id = k.to_topic
              ORDER BY k.corrected_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup corrections", source))?
        .into_iter()
        .map(CorrectionRow::correction)
        .collect())
    }

    async fn snapshot_redirects(&self) -> Result<Vec<SnapshotRedirect>, Error> {
        Ok(
            sqlx::query_as::<_, RedirectRow>("SELECT slug, topic_id FROM rh_topic_redirects")
                .fetch_all(&self.pool)
                .await
                .map_err(|source| Error::operation("cleanup redirects", source))?
                .into_iter()
                .map(|row| SnapshotRedirect {
                    slug: row.slug,
                    topic: TopicId::from_uuid(row.topic_id),
                })
                .collect(),
        )
    }

    async fn snapshot_projects(&self) -> Result<(Vec<Project>, Vec<String>), Error> {
        let rows = sqlx::query_as::<_, TaxonomyRow>(
            "SELECT value, aliases, state FROM rh_tag_taxonomy
              WHERE key = 'project' ORDER BY created_at, value",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup projects", source))?
        .into_iter()
        .map(|row| {
            let state = TagState::from_str(&row.state)
                .map_err(|source| Error::stored("tag state", source))?;
            Ok((row, state))
        })
        .collect::<Result<Vec<_>, Error>>()?;
        let mut refused: Vec<String> = rows
            .iter()
            .filter(|(_, state)| *state == TagState::Rejected)
            .map(|(row, _)| row.value.clone())
            .collect();
        if self.has_table("rh_topic_tombstones").await? {
            refused.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT slug FROM rh_topic_tombstones WHERE kind = 'work'",
                )
                .fetch_all(&self.pool)
                .await
                .map_err(|source| Error::operation("cleanup tombstones", source))?,
            );
        }
        let projects = rows
            .into_iter()
            .filter(|(_, state)| *state == TagState::Accepted)
            .map(|(row, _)| Project {
                name: row.value,
                aliases: row.aliases,
            })
            .collect();
        Ok((projects, refused))
    }

    async fn read_snapshot(&self) -> Result<Snapshot, Error> {
        self.ensure_read_only().await?;
        let taken_at = Timestamp::now();
        let (projects, refused) = self.snapshot_projects().await?;
        Ok(Snapshot {
            taken_at,
            conversations: self.snapshot_conversations().await?,
            topics: self.snapshot_topics().await?,
            redirects: self.snapshot_redirects().await?,
            placements: self.snapshot_placements().await?,
            corrections: self.snapshot_corrections().await?,
            projects,
            refused,
            pages: self.snapshot_pages().await?,
            sections: self.snapshot_sections().await?,
        })
    }
}

#[async_trait]
impl CleanupSource for PostgresMemory {
    async fn snapshot(&self) -> Result<Snapshot, CleanupError> {
        self.read_snapshot()
            .await
            .map_err(|error| CleanupError::Snapshot(Box::new(error)))
    }
}
