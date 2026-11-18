use jiff::Timestamp;
use std::str::FromStr;

use async_trait::async_trait;
use kernel::filing::AllFiled;
use kernel::filing::ConversationPlacement;
use kernel::filing::Correction;
use kernel::filing::CorrectionExample;
use kernel::filing::Filed;
use kernel::filing::FilingStore;
use kernel::filing::Mention;
use kernel::filing::Subject;
use kernel::filing::Target;
use kernel::filing::TopicLine;
use kernel::memory::ConversationHead;
use kernel::topics::PlacementSource;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use sqlx::PgConnection;
use uuid::Uuid;

use super::rows::failed;
use super::rows::not_found;
use super::rows::stored;
use crate::PostgresMemory;

struct ConversationPlacementRow {
    conversation_id: Uuid,
    topic_id: Uuid,
    placed_by: String,
    placed_at: Timestamp,
}

struct HeadRow {
    id: Uuid,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
}

#[async_trait]
impl FilingStore for PostgresMemory {
    async fn subject(&self, conversation: Uuid) -> Result<Option<Subject>, TopicError> {
        #[derive(sqlx::FromRow)]
        struct SubjectRow {
            id: Uuid,
            session_key: String,
            agent: String,
            channel: String,
            title: Option<String>,
            cwd: Option<String>,
            summary: Option<String>,
        }
        let row = sqlx::query_as::<_, SubjectRow>(
            "SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                    c.settings->>'cwd' AS cwd, o.content AS summary
               FROM rh_conversations c
               LEFT JOIN rh_summary_openings o ON o.conversation_id = c.id
              WHERE c.id = $1",
        )
        .bind(conversation)
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("filing subject"))?;
        Ok(row.map(|row| Subject {
            head: ConversationHead {
                id: row.id,
                session_key: row.session_key,
                agent: row.agent,
                channel: row.channel,
                title: row.title,
            },
            cwd: row.cwd.filter(|cwd| !cwd.trim().is_empty()),
            summary: row.summary,
        }))
    }

    async fn task_parent(
        &self,
        task: Uuid,
        conversation: Uuid,
    ) -> Result<Option<ConversationHead>, TopicError> {
        let named = format!("%{task}%");
        let row = sqlx::query_as!(
            HeadRow,
            "WITH named AS (
                 SELECT m.conversation_id, m.created_at, true AS tool_output FROM rh_messages m
                  WHERE m.tool_result IS NOT NULL AND length(m.tool_result) > 0
                    AND m.tool_result LIKE $1
                 UNION ALL
                 SELECT m.conversation_id, m.created_at, false FROM rh_messages m
                  WHERE m.content LIKE $1
             )
             SELECT c.id, c.session_key, c.agent, c.channel, c.title
               FROM named n
               JOIN rh_conversations c ON c.id = n.conversation_id
              WHERE n.conversation_id <> $2
              ORDER BY n.tool_output DESC, n.created_at, c.id
              LIMIT 1",
            named,
            conversation
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("task parent"))?;
        Ok(row.map(|row| ConversationHead {
            id: row.id,
            session_key: row.session_key,
            agent: row.agent,
            channel: row.channel,
            title: row.title,
        }))
    }

    async fn conversation_placement(
        &self,
        conversation: Uuid,
    ) -> Result<Option<ConversationPlacement>, TopicError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(failed("conversation placement read"))?;
        placement_of(&mut conn, conversation, false).await
    }

    async fn place_conversation(
        &self,
        placement: ConversationPlacement,
    ) -> Result<ConversationPlacement, TopicError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(failed("conversation placement"))?;
        super::rows::by_id(&mut tx, placement.topic)
            .await?
            .ok_or_else(|| not_found(placement.topic))?
            .accepts_sessions()?;
        let existing = placement_of(&mut tx, placement.conversation, true).await?;
        let kept = placement.clone().admit(existing);
        if kept == placement {
            sqlx::query!(
                "INSERT INTO rh_conversation_placements (conversation_id, topic_id, placed_by, placed_at, owner_user_id)
                 VALUES ($1, $2, $3, $4, (SELECT owner_user_id FROM rh_conversations WHERE id = $1))
                 ON CONFLICT (conversation_id) DO UPDATE SET topic_id = EXCLUDED.topic_id,
                                                             placed_by = EXCLUDED.placed_by,
                                                             placed_at = EXCLUDED.placed_at",
                placement.conversation,
                placement.topic.uuid(),
                placement.by.to_string(),
            crate::time::BindTime::bind_time(placement.at) as _)
            .execute(&mut *tx)
            .await
            .map_err(failed("conversation placement write"))?;
        }
        tx.commit()
            .await
            .map_err(failed("conversation placement"))?;
        Ok(kept)
    }

    async fn unplace_conversation(
        &self,
        conversation: Uuid,
    ) -> Result<Option<ConversationPlacement>, TopicError> {
        sqlx::query_as!(
            ConversationPlacementRow,
            "DELETE FROM rh_conversation_placements WHERE conversation_id = $1
             RETURNING conversation_id, topic_id, placed_by, placed_at AS \"placed_at: jiff_sqlx::Timestamp\"",
            conversation
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(failed("conversation unplace"))?
        .map(|row| conversation_placement(&row))
        .transpose()
    }

    async fn filed(&self, topic: TopicId) -> Result<Filed, TopicError> {
        super::bulk::filed(&self.pool, topic).await
    }

    async fn all_filed(&self) -> Result<AllFiled, TopicError> {
        super::bulk::all_filed(&self.pool).await
    }

    async fn mentions(&self, topics: &[TopicId], limit: u32) -> Result<Vec<Mention>, TopicError> {
        super::mentions::read(&self.pool, topics, limit).await
    }

    async fn corrected(&self, target: &Target) -> Result<bool, TopicError> {
        let (session, conversation) = match target {
            Target::Session(session) => (Some(session.as_str()), None),
            Target::Conversation(conversation) => (None, Some(*conversation)),
        };
        sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM rh_filing_corrections
                               WHERE session_id = $1 OR conversation_id = $2) AS "corrected!""#,
            session,
            conversation
        )
        .fetch_one(&self.pool)
        .await
        .map_err(failed("filing corrected"))
    }

    async fn delegated(&self, target: &Target) -> Result<bool, TopicError> {
        let Target::Session(session) = target else {
            return Ok(false);
        };
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rh_tasks WHERE child_session_id = $1)")
            .bind(session.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(failed("delegated session"))
    }

    async fn correct(&self, correction: Correction) -> Result<(), TopicError> {
        let (session, conversation) = match &correction.target {
            Target::Session(session) => (Some(session.as_str().to_owned()), None),
            Target::Conversation(conversation) => (None, Some(*conversation)),
        };
        sqlx::query!(
            "INSERT INTO rh_filing_corrections
                 (id, session_id, conversation_id, from_topic, to_topic, corrected_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            Uuid::new_v4(),
            session,
            conversation,
            correction.from.uuid(),
            correction.to.map(TopicId::uuid),
            crate::time::BindTime::bind_time(correction.at) as _
        )
        .execute(&self.pool)
        .await
        .map_err(failed("filing correction"))?;
        Ok(())
    }

    async fn corrections(&self, limit: u32) -> Result<Vec<CorrectionExample>, TopicError> {
        sqlx::query!(
            r#"SELECT f.name AS from_name, t.name AS "to_name?",
                      COALESCE(
                          (SELECT c.title FROM rh_conversations c WHERE c.id = k.conversation_id),
                          (SELECT c.title FROM rh_conversations c
                            WHERE c.session_key = k.session_id
                            ORDER BY c.updated_at DESC LIMIT 1)
                      ) AS title
                 FROM rh_filing_corrections k
                 JOIN rh_topics f ON f.id = k.from_topic
                 LEFT JOIN rh_topics t ON t.id = k.to_topic
                ORDER BY k.corrected_at DESC
                LIMIT $1"#,
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("filing corrections"))?
        .into_iter()
        .map(|row| {
            Ok(CorrectionExample {
                title: row.title,
                from: TopicName::new(&row.from_name)?,
                to: row.to_name.as_deref().map(TopicName::new).transpose()?,
            })
        })
        .collect()
    }

    async fn topic_lines(&self) -> Result<Vec<TopicLine>, TopicError> {
        Ok(sqlx::query!(
            r#"SELECT topic_id AS "topic_id!", current_state
                 FROM rh_wiki_topics WHERE topic_id IS NOT NULL"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed("topic lines"))?
        .into_iter()
        .filter_map(|row| {
            TopicLine::from_page(TopicId::from_uuid(row.topic_id), &row.current_state)
        })
        .collect())
    }
}

pub(super) async fn fold_into(
    conn: &mut PgConnection,
    source: Uuid,
    into: Uuid,
) -> Result<(), TopicError> {
    sqlx::query!(
        "WITH placed AS (
             UPDATE rh_conversation_placements SET topic_id = $2 WHERE topic_id = $1
         )
         UPDATE rh_session_suggestions SET parent_id = $2 WHERE parent_id = $1",
        source,
        into
    )
    .execute(&mut *conn)
    .await
    .map_err(failed("topic merge filings"))?;
    Ok(())
}

async fn placement_of(
    conn: &mut PgConnection,
    conversation: Uuid,
    lock: bool,
) -> Result<Option<ConversationPlacement>, TopicError> {
    let row = if lock {
        sqlx::query_as!(
            ConversationPlacementRow,
            "SELECT conversation_id, topic_id, placed_by, placed_at AS \"placed_at: jiff_sqlx::Timestamp\"
               FROM rh_conversation_placements WHERE conversation_id = $1 FOR UPDATE",
            conversation
        )
        .fetch_optional(&mut *conn)
        .await
    } else {
        sqlx::query_as!(
            ConversationPlacementRow,
            "SELECT conversation_id, topic_id, placed_by, placed_at AS \"placed_at: jiff_sqlx::Timestamp\"
               FROM rh_conversation_placements WHERE conversation_id = $1",
            conversation
        )
        .fetch_optional(&mut *conn)
        .await
    };
    row.map_err(failed("conversation placement read"))?
        .map(|row| conversation_placement(&row))
        .transpose()
}

fn conversation_placement(
    row: &ConversationPlacementRow,
) -> Result<ConversationPlacement, TopicError> {
    Ok(ConversationPlacement {
        conversation: row.conversation_id,
        topic: TopicId::from_uuid(row.topic_id),
        by: PlacementSource::from_str(&row.placed_by)
            .map_err(|error| stored("placed_by")(Box::new(error)))?,
        at: row.placed_at,
    })
}
