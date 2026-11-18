use jiff::Timestamp;
use std::cmp::Reverse;
use std::str::FromStr;

use kernel::filing::AllFiled;
use kernel::filing::ConversationPlacement;
use kernel::filing::Filed;
use kernel::filing::FiledConversation;
use kernel::filing::FiledSession;
use kernel::filing::bare_session_prefix;
use kernel::topics::HistoryMark;
use kernel::topics::PlacementSource;
use kernel::topics::Suggestion;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use sqlx::PgPool;
use uuid::Uuid;
use wire::session_id::SessionId;

use super::rows;
use super::rows::PlacementRow;
use super::rows::SuggestionRow;
use super::rows::failed;
use super::rows::stored;

struct FilingRow {
    session_id: Option<String>,
    conversation_id: Option<Uuid>,
    topic_id: Option<Uuid>,
    placed_by: Option<String>,
    at: Timestamp,
    title: Option<String>,
    channel: Option<String>,
    last_active: Option<Timestamp>,
    node: Option<String>,
    owner_user_id: Option<String>,
    session_owner: Option<String>,
    mark: Option<String>,
    effective: Option<String>,
}

async fn filings(pool: &PgPool, topic: Option<TopicId>) -> Result<AllFiled, TopicError> {
    sqlx::query_as!(
        FilingRow,
        r#"WITH placed AS (
               SELECT p.session_id, p.topic_id, p.placed_by, p.placed_at, p.owner_user_id,
                      CASE WHEN starts_with(p.session_id, $1)
                           THEN substr(p.session_id, length($1) + 1) END AS bare
                 FROM rh_session_placements p
                WHERE $2::uuid IS NULL OR p.topic_id = $2
           ),
           keyed AS (
               SELECT session_id, session_id AS key FROM placed
               UNION ALL
               SELECT session_id, bare FROM placed WHERE bare IS NOT NULL
           ),
           seen AS (
               SELECT k.session_id, max(c.updated_at) AS last_active,
                      (array_agg(c.title ORDER BY c.session_key = k.session_id DESC, c.updated_at DESC)
                         FILTER (WHERE c.title IS NOT NULL))[1] AS title,
                      (array_agg(c.node ORDER BY c.session_key = k.session_id DESC, c.updated_at DESC)
                         FILTER (WHERE c.node IS NOT NULL))[1] AS node,
                      (array_agg(c.owner_user_id ORDER BY c.created_at)
                         FILTER (WHERE c.session_key = k.session_id))[1] AS owner,
                      (array_agg(c.visibility ORDER BY c.created_at)
                         FILTER (WHERE c.session_key = k.session_id))[1] AS visibility
                 FROM keyed k
                 JOIN rh_conversations c ON c.session_key = k.key
                GROUP BY k.session_id
           )
           SELECT p.session_id AS "session_id?", NULL::uuid AS "conversation_id?",
                  p.topic_id AS "topic_id?", p.placed_by AS "placed_by?", p.placed_at AS "at!: jiff_sqlx::Timestamp",
                  seen.title AS "title?", NULL::text AS "channel?",
                  seen.last_active AS "last_active!: crate::time::OptionalTimestamp", seen.node AS "node?",
                  p.owner_user_id AS "owner_user_id?", seen.owner AS "session_owner?",
                  m.visibility AS "mark?",
                  coalesce(seen.visibility,
                           rh_member_visibility(rh_scope_principal(),
                               coalesce(m.visibility, rh_topic_mark(p.topic_id),
                                        rh_owner_default(rh_scope_principal(), now())))) AS "effective?"
             FROM placed p
             LEFT JOIN seen ON seen.session_id = p.session_id
             LEFT JOIN rh_session_marks m
                    ON m.session_key = p.session_id AND m.owner_user_id = rh_scope_principal()
           UNION ALL
           SELECT NULL, p.conversation_id, p.topic_id, p.placed_by, p.placed_at,
                  c.title, c.channel, c.updated_at, c.node, p.owner_user_id, NULL, NULL, NULL
             FROM rh_conversation_placements p
             JOIN rh_conversations c ON c.id = p.conversation_id
            WHERE $2::uuid IS NULL OR p.topic_id = $2
           UNION ALL
           SELECT h.session_id, NULL, NULL, NULL, h.marked_at, NULL, NULL, NULL, NULL, NULL,
                  NULL, NULL, NULL
             FROM rh_session_history h
            WHERE $2::uuid IS NULL
           ORDER BY 8 DESC NULLS LAST, 5 DESC, 1, 2"#,
        bare_session_prefix(),
        topic.map(TopicId::uuid)
    )
    .fetch_all(pool)
    .await
    .map_err(failed("filed sessions"))?
    .into_iter()
    .try_fold(AllFiled::default(), |mut filed, row| {
        match (row.topic_id, row.conversation_id) {
            (Some(topic), Some(conversation)) => {
                filed
                    .conversations
                    .push(conversation_row(row, topic, conversation)?);
            }
            (Some(topic), None) => filed.sessions.push(session_row(row, topic)?),
            (None, None) => filed.history.push(HistoryMark {
                session: session_id(row.session_id)?,
                at: row.at,
            }),
            (None, Some(_)) => return Err(missing("topic_id")),
        }
        Ok(filed)
    })
}

pub(super) async fn all_filed(pool: &PgPool) -> Result<AllFiled, TopicError> {
    let mut filed = filings(pool, None).await?;
    filed
        .sessions
        .extend(super::inherited::sessions(pool, None).await?);
    Ok(filed)
}

pub(super) async fn filed(pool: &PgPool, topic: TopicId) -> Result<Filed, TopicError> {
    let AllFiled {
        mut sessions,
        conversations,
        ..
    } = filings(pool, Some(topic)).await?;
    sessions.extend(super::inherited::sessions(pool, Some(topic)).await?);
    sessions.sort_by_key(|filed| Reverse(filed.last_active));
    Ok(Filed {
        sessions,
        conversations,
    })
}

pub(super) async fn suggestions(pool: &PgPool) -> Result<Vec<Suggestion>, TopicError> {
    sqlx::query_as!(
        SuggestionRow,
        "SELECT s.session_id, s.name, s.topic_id, s.parent_id
           FROM rh_session_suggestions s
          WHERE NOT EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = s.session_id)
          ORDER BY s.session_id"
    )
    .fetch_all(pool)
    .await
    .map_err(failed("session suggestions"))?
    .into_iter()
    .map(rows::suggestion)
    .collect()
}

fn session_row(row: FilingRow, topic: Uuid) -> Result<FiledSession, TopicError> {
    Ok(FiledSession {
        placement: rows::placement(PlacementRow {
            session_id: row.session_id.ok_or_else(|| missing("session_id"))?,
            topic_id: topic,
            placed_by: row.placed_by.ok_or_else(|| missing("placed_by"))?,
            placed_at: row.at,
            owner_user_id: row.owner_user_id.ok_or_else(|| missing("owner_user_id"))?,
        })?,
        title: row.title,
        last_active: row.last_active,
        node: row.node,
        lineage: None,
        visibility: rows::session_visibility(
            row.session_owner,
            row.mark.as_deref(),
            row.effective,
        )?,
    })
}

fn conversation_row(
    row: FilingRow,
    topic: Uuid,
    conversation: Uuid,
) -> Result<FiledConversation, TopicError> {
    let placed_by = row.placed_by.ok_or_else(|| missing("placed_by"))?;
    Ok(FiledConversation {
        placement: ConversationPlacement {
            conversation,
            topic: TopicId::from_uuid(topic),
            by: PlacementSource::from_str(&placed_by)
                .map_err(|error| stored("placed_by")(Box::new(error)))?,
            at: row.at,
        },
        title: row.title,
        channel: row.channel.ok_or_else(|| missing("channel"))?,
        last_active: row.last_active,
        node: row.node,
    })
}

fn session_id(id: Option<String>) -> Result<SessionId, TopicError> {
    SessionId::try_from(id.ok_or_else(|| missing("session_id"))?)
        .map_err(|error| stored("session_id")(Box::new(error)))
}

fn missing(field: &'static str) -> TopicError {
    stored(field)("a filed row without it".into())
}
