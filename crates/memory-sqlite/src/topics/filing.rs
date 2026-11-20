use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::str::FromStr;

use async_trait::async_trait;
use kernel::filing::AllFiled;
use kernel::filing::ConversationPlacement;
use kernel::filing::Correction;
use kernel::filing::CorrectionExample;
use kernel::filing::Filed;
use kernel::filing::FiledConversation;
use kernel::filing::FiledSession;
use kernel::filing::FilingStore;
use kernel::filing::Mention;
use kernel::filing::Subject;
use kernel::filing::Target;
use kernel::filing::TopicLine;
use kernel::filing::conversation_keys;
use kernel::memory::ConversationHead;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicName;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use uuid::Uuid;

use super::rows;
use super::rows::PLACEMENT_COLUMNS;
use super::rows::failed;
use super::rows::not_found;
use super::rows::stored;
use super::rows::timestamp;
use super::rows::topic_id;
use crate::SqliteStore;
use crate::time::stamp;

const PLACED_COLUMNS: &str = "conversation_id, topic_id, placed_by, placed_at";

struct Seen {
    title: Option<String>,
    last_active: Option<String>,
    node: Option<String>,
}

#[async_trait]
impl FilingStore for SqliteStore {
    async fn subject(&self, conversation: Uuid) -> Result<Option<Subject>, TopicError> {
        self.topics("filing subject", move |conn| {
            conn.query_row(
                "SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                        json_extract(c.settings, '$.cwd'),
                        o.content
                   FROM ros_conversations c
                   LEFT JOIN rh_summary_openings o ON o.conversation_id = c.id
                  WHERE c.id = ?1",
                params![conversation.to_string()],
                |row| {
                    Ok((
                        read_head(row)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(failed("filing subject"))?
            .map(|(head, cwd, summary)| {
                Ok(Subject {
                    head: head_of(head)?,
                    cwd: cwd.filter(|cwd| !cwd.trim().is_empty()),
                    summary,
                })
            })
            .transpose()
        })
        .await
    }

    async fn task_parent(
        &self,
        task: Uuid,
        conversation: Uuid,
    ) -> Result<Option<ConversationHead>, TopicError> {
        self.topics("task parent", move |conn| {
            conn.query_row(
                "SELECT c.id, c.session_key, c.agent, c.channel, c.title
                   FROM ros_messages m
                   JOIN ros_conversations c ON c.id = m.conversation_id
                  WHERE m.conversation_id <> ?2
                    AND (instr(coalesce(m.tool_result, ''), ?1) > 0 OR instr(m.content, ?1) > 0)
                  ORDER BY instr(coalesce(m.tool_result, ''), ?1) > 0 DESC,
                           julianday(m.created_at), c.id
                  LIMIT 1",
                params![task.to_string(), conversation.to_string()],
                read_head,
            )
            .optional()
            .map_err(failed("task parent"))?
            .map(head_of)
            .transpose()
        })
        .await
    }

    async fn conversation_placement(
        &self,
        conversation: Uuid,
    ) -> Result<Option<ConversationPlacement>, TopicError> {
        self.topics("conversation placement read", move |conn| {
            placement_of(conn, conversation)
        })
        .await
    }

    async fn place_conversation(
        &self,
        placement: ConversationPlacement,
    ) -> Result<ConversationPlacement, TopicError> {
        self.topics("conversation placement", move |conn| {
            rows::by_id(conn, placement.topic)?
                .ok_or_else(|| not_found(placement.topic))?
                .accepts_sessions()?;
            let existing = placement_of(conn, placement.conversation)?;
            let kept = placement.clone().admit(existing);
            if kept == placement {
                conn.execute(
                    "INSERT INTO rh_conversation_placements (conversation_id, topic_id, placed_by, placed_at, owner_user_id)
                     VALUES (?1, ?2, ?3, ?4, (SELECT owner_user_id FROM ros_conversations WHERE id = ?1))
                     ON CONFLICT (conversation_id) DO UPDATE SET topic_id = excluded.topic_id,
                                                                 placed_by = excluded.placed_by,
                                                                 placed_at = excluded.placed_at",
                    params![
                        placement.conversation.to_string(),
                        placement.topic.to_string(),
                        placement.by.to_string(),
                        stamp(placement.at),
                    ],
                )
                .map_err(failed("conversation placement write"))?;
            }
            Ok(kept)
        })
        .await
    }

    async fn unplace_conversation(
        &self,
        conversation: Uuid,
    ) -> Result<Option<ConversationPlacement>, TopicError> {
        self.topics("conversation unplace", move |conn| {
            let existing = placement_of(conn, conversation)?;
            conn.execute(
                "DELETE FROM rh_conversation_placements WHERE conversation_id = ?1",
                params![conversation.to_string()],
            )
            .map_err(failed("conversation unplace"))?;
            Ok(existing)
        })
        .await
    }

    async fn filed(&self, topic: TopicId) -> Result<Filed, TopicError> {
        self.topics("filed", move |conn| {
            let sessions = filed_sessions(conn, topic)?;
            let conversations = filed_conversations(conn, topic)?;
            Ok(Filed {
                sessions,
                conversations,
            })
        })
        .await
    }

    async fn all_filed(&self) -> Result<AllFiled, TopicError> {
        self.topics("all filed", super::bulk::all_filed).await
    }

    async fn mentions(&self, topics: &[TopicId], limit: u32) -> Result<Vec<Mention>, TopicError> {
        let topics = topics.to_vec();
        self.topics("topic mentions", move |conn| {
            super::mentions::read(conn, &topics, limit)
        })
        .await
    }

    async fn corrected(&self, target: &Target) -> Result<bool, TopicError> {
        let (session, conversation) = match target {
            Target::Session(session) => (Some(session.as_str().to_owned()), None),
            Target::Conversation(conversation) => (None, Some(conversation.to_string())),
        };
        self.topics("filing corrected", move |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM rh_filing_corrections
                                 WHERE session_id = ?1 OR conversation_id = ?2)",
                params![session, conversation],
                |row| row.get(0),
            )
            .map_err(failed("filing corrected"))
        })
        .await
    }

    async fn delegated(&self, target: &Target) -> Result<bool, TopicError> {
        let Target::Session(session) = target else {
            return Ok(false);
        };
        let session = session.as_str().to_owned();
        self.topics("delegated session", move |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM rh_tasks WHERE child_session_id = ?1)",
                params![session],
                |row| row.get(0),
            )
            .map_err(failed("delegated session"))
        })
        .await
    }

    async fn correct(&self, correction: Correction) -> Result<(), TopicError> {
        self.topics("filing correction", move |conn| {
            let (session, conversation) = match &correction.target {
                Target::Session(session) => (Some(session.as_str().to_owned()), None),
                Target::Conversation(conversation) => (None, Some(conversation.to_string())),
            };
            conn.execute(
                "INSERT INTO rh_filing_corrections
                     (id, session_id, conversation_id, from_topic, to_topic, corrected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    session,
                    conversation,
                    correction.from.to_string(),
                    correction.to.map(|topic| topic.to_string()),
                    stamp(correction.at),
                ],
            )
            .map_err(failed("filing correction"))?;
            Ok(())
        })
        .await
    }

    async fn corrections(&self, limit: u32) -> Result<Vec<CorrectionExample>, TopicError> {
        self.topics("filing corrections", move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT f.name, t.name,
                            coalesce(
                                (SELECT c.title FROM ros_conversations c WHERE c.id = k.conversation_id),
                                (SELECT c.title FROM ros_conversations c
                                  WHERE c.session_key = k.session_id
                                  ORDER BY c.updated_at DESC LIMIT 1))
                       FROM rh_filing_corrections k
                       JOIN rh_topics f ON f.id = k.from_topic
                       LEFT JOIN rh_topics t ON t.id = k.to_topic
                      ORDER BY k.corrected_at DESC
                      LIMIT ?1",
                )
                .map_err(failed("filing corrections"))?;
            let found = statement
                .query_map(params![limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .map_err(failed("filing corrections"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(failed("filing corrections"))?;
            found
                .into_iter()
                .map(|(from, to, title)| {
                    Ok(CorrectionExample {
                        title,
                        from: TopicName::new(&from)?,
                        to: to.as_deref().map(TopicName::new).transpose()?,
                    })
                })
                .collect()
        })
        .await
    }

    async fn topic_lines(&self) -> Result<Vec<TopicLine>, TopicError> {
        self.topics("topic lines", move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT p.topic_id, w.current_state
                       FROM rh_topic_pages p
                       JOIN ros_wiki_topics w ON w.slug = p.page_slug",
                )
                .map_err(failed("topic lines"))?;
            let pages = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(failed("topic lines"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(failed("topic lines"))?;
            let mut lines = Vec::new();
            for (topic, page) in pages {
                if let Some(line) = TopicLine::from_page(topic_id(&topic)?, &page) {
                    lines.push(line);
                }
            }
            Ok(lines)
        })
        .await
    }
}

pub(super) fn fold_into(conn: &Connection, source: &str, into: &str) -> Result<(), TopicError> {
    conn.execute(
        "UPDATE rh_conversation_placements SET topic_id = ?2 WHERE topic_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge conversation placements"))?;
    conn.execute(
        "UPDATE rh_session_suggestions SET parent_id = ?2 WHERE parent_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge suggestion parents"))?;
    Ok(())
}

type HeadColumns = (String, String, String, String, Option<String>);

fn read_head(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeadColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn head_of(
    (id, session_key, agent, channel, title): HeadColumns,
) -> Result<ConversationHead, TopicError> {
    Ok(ConversationHead {
        id: Uuid::parse_str(&id).map_err(|error| stored("conversation id")(Box::new(error)))?,
        session_key,
        agent,
        channel,
        title,
        trashed: false,
    })
}

fn placement_of(
    conn: &Connection,
    conversation: Uuid,
) -> Result<Option<ConversationPlacement>, TopicError> {
    conn.query_row(
        &format!(
            "SELECT {PLACED_COLUMNS} FROM rh_conversation_placements WHERE conversation_id = ?1"
        ),
        params![conversation.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )
    .optional()
    .map_err(failed("conversation placement read"))?
    .map(conversation_placement)
    .transpose()
}

pub(super) fn conversation_placement(
    (conversation, topic, by, at): (String, String, String, String),
) -> Result<ConversationPlacement, TopicError> {
    Ok(ConversationPlacement {
        conversation: Uuid::parse_str(&conversation)
            .map_err(|error| stored("conversation id")(Box::new(error)))?,
        topic: topic_id(&topic)?,
        by: PlacementSource::from_str(&by).map_err(|error| stored("placed_by")(Box::new(error)))?,
        at: timestamp(&at, "placed_at")?,
    })
}

fn filed_sessions(conn: &Connection, topic: TopicId) -> Result<Vec<FiledSession>, TopicError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {PLACEMENT_COLUMNS} FROM rh_session_placements p WHERE p.topic_id = ?1"
        ))
        .map_err(failed("filed sessions"))?;
    let placed = statement
        .query_map(params![topic.to_string()], rows::read_placement)
        .map_err(failed("filed sessions"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed("filed sessions"))?
        .into_iter()
        .map(rows::placement)
        .collect::<Result<Vec<Placement>, _>>()?;
    let mut seen = BTreeMap::new();
    let mut lookup = conn
        .prepare(
            "SELECT title, max(updated_at) OVER (),
                    (SELECT n.node FROM ros_conversations k
                       JOIN rh_conversation_nodes n ON n.conversation_id = k.id
                      WHERE k.session_key = ?1
                      ORDER BY k.updated_at DESC LIMIT 1)
               FROM ros_conversations
              WHERE session_key = ?1
              ORDER BY title IS NULL, updated_at DESC
              LIMIT 1",
        )
        .map_err(failed("filed session titles"))?;
    for key in placed
        .iter()
        .flat_map(|placement| conversation_keys(&placement.session))
    {
        let found = lookup
            .query_row(params![key], |row| {
                Ok(Seen {
                    title: row.get(0)?,
                    last_active: row.get(1)?,
                    node: row.get(2)?,
                })
            })
            .optional()
            .map_err(failed("filed session titles"))?;
        if let Some(found) = found {
            seen.insert(key, found);
        }
    }
    let mut sessions = Vec::with_capacity(placed.len());
    for placement in placed {
        let found: Vec<&Seen> = conversation_keys(&placement.session)
            .iter()
            .filter_map(|key| seen.get(key))
            .collect();
        let mut last_active = None;
        for at in found.iter().filter_map(|row| row.last_active.as_deref()) {
            let at = timestamp(at, "updated_at")?;
            last_active = last_active.max(Some(at));
        }
        sessions.push(FiledSession {
            title: found.iter().find_map(|row| row.title.clone()),
            last_active,
            node: found.iter().find_map(|row| row.node.clone()),
            placement,
            lineage: None,
            visibility: None,
        });
    }
    sessions.extend(super::inherited::sessions(conn, Some(topic))?);
    sessions.sort_by_key(|filed| Reverse(filed.last_active));
    Ok(sessions)
}

fn filed_conversations(
    conn: &Connection,
    topic: TopicId,
) -> Result<Vec<FiledConversation>, TopicError> {
    let mut statement = conn
        .prepare(
            "SELECT p.conversation_id, p.topic_id, p.placed_by, p.placed_at,
                    c.title, c.channel, c.updated_at, n.node
               FROM rh_conversation_placements p
               JOIN ros_conversations c ON c.id = p.conversation_id
               LEFT JOIN rh_conversation_nodes n ON n.conversation_id = c.id
              WHERE p.topic_id = ?1",
        )
        .map_err(failed("filed conversations"))?;
    let found = statement
        .query_map(params![topic.to_string()], |row| {
            Ok((
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?),
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(failed("filed conversations"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failed("filed conversations"))?;
    let mut conversations = found
        .into_iter()
        .map(|(placed, title, channel, updated, node)| {
            Ok(FiledConversation {
                placement: conversation_placement(placed)?,
                title,
                channel,
                last_active: Some(timestamp(&updated, "updated_at")?),
                node,
            })
        })
        .collect::<Result<Vec<_>, TopicError>>()?;
    conversations.sort_by_key(|filed| Reverse(filed.last_active));
    Ok(conversations)
}
