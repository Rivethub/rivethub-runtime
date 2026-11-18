use jiff::Timestamp;
use std::str::FromStr;

use kernel::filing::FiledSession;
use kernel::filing::bare_session_prefix;
use kernel::topics::Lineage;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;
use wire::session_id::SessionId;

use super::rows::failed;
use super::rows::stored;

const INHERITED: &str = r"
WITH RECURSIVE inherited AS (
    SELECT t.child_session_id AS session_id, t.parent_session_id AS parent, t.id AS task,
           t.node, p.topic_id, p.placed_by, p.placed_at, t.owner_user_id, 1 AS depth
      FROM rh_tasks t
      JOIN rh_session_placements p ON p.session_id = t.parent_session_id
     WHERE t.child_session_id IS NOT NULL AND ($1::uuid IS NULL OR p.topic_id = $1)
    UNION ALL
    SELECT t.child_session_id, t.parent_session_id, t.id, t.node, i.topic_id, i.placed_by,
           i.placed_at, t.owner_user_id, i.depth + 1
      FROM rh_tasks t
      JOIN inherited i ON t.parent_session_id = i.session_id
     WHERE t.child_session_id IS NOT NULL AND i.depth < 8
       AND NOT EXISTS (SELECT 1 FROM rh_session_placements own WHERE own.session_id = i.session_id)
)
SELECT i.session_id, i.parent, i.task, i.node, i.topic_id, i.placed_by, i.placed_at,
       i.owner_user_id, seen.title, seen.last_active, seen.owner AS session_owner,
       m.visibility AS mark,
       coalesce(seen.visibility,
                rh_member_visibility(rh_scope_principal(),
                    coalesce(m.visibility, rh_owner_default(rh_scope_principal(), now())))) AS effective
  FROM inherited i
  LEFT JOIN LATERAL (
      SELECT max(c.updated_at) AS last_active,
             (array_agg(c.title ORDER BY c.updated_at DESC)
                FILTER (WHERE c.title IS NOT NULL))[1] AS title,
             (array_agg(c.owner_user_id ORDER BY c.created_at)
                FILTER (WHERE c.session_key = i.session_id))[1] AS owner,
             (array_agg(c.visibility ORDER BY c.created_at)
                FILTER (WHERE c.session_key = i.session_id))[1] AS visibility
        FROM rh_conversations c
       WHERE c.session_key IN (i.session_id,
             CASE WHEN starts_with(i.session_id, $2)
                  THEN substr(i.session_id, length($2) + 1) END)
  ) seen ON true
  LEFT JOIN rh_session_marks m
         ON m.session_key = i.session_id AND m.owner_user_id = rh_scope_principal()
 WHERE NOT EXISTS (SELECT 1 FROM rh_session_placements own WHERE own.session_id = i.session_id)
";

pub(super) async fn sessions(
    pool: &PgPool,
    topic: Option<TopicId>,
) -> Result<Vec<FiledSession>, TopicError> {
    let rows = sqlx::query(INHERITED)
        .bind(topic.map(TopicId::uuid))
        .bind(bare_session_prefix())
        .fetch_all(pool)
        .await
        .map_err(failed("inherited sessions"))?;
    rows.iter().map(session).collect()
}

fn session(row: &sqlx::postgres::PgRow) -> Result<FiledSession, TopicError> {
    let read = |column: &'static str| failed(column);
    let child: String = row.try_get("session_id").map_err(read("session_id"))?;
    let parent: String = row.try_get("parent").map_err(read("parent"))?;
    let placed_by: String = row.try_get("placed_by").map_err(read("placed_by"))?;
    let at: Timestamp = row
        .try_get::<jiff_sqlx::Timestamp, _>("placed_at")
        .map(jiff_sqlx::Timestamp::to_jiff)
        .map_err(read("placed_at"))?;
    let owner: String = row
        .try_get("owner_user_id")
        .map_err(read("owner_user_id"))?;
    Ok(FiledSession {
        placement: Placement {
            session: id(child, "session_id")?,
            topic: TopicId::from_uuid(
                row.try_get::<Uuid, _>("topic_id")
                    .map_err(read("topic_id"))?,
            ),
            by: PlacementSource::from_str(&placed_by)
                .map_err(|error| stored("placed_by")(Box::new(error)))?,
            at,
            owner: Some(super::rows::owner(owner)?),
        },
        title: row.try_get("title").map_err(read("title"))?,
        last_active: row
            .try_get::<crate::time::OptionalTimestamp, _>("last_active")
            .map(Option::from)
            .map_err(read("last_active"))?,
        node: Some(row.try_get("node").map_err(read("node"))?),
        lineage: Some(Lineage {
            parent: id(parent, "parent")?,
            task: row.try_get("task").map_err(read("task"))?,
        }),
        visibility: super::rows::session_visibility(
            row.try_get("session_owner")
                .map_err(read("session_owner"))?,
            row.try_get::<Option<String>, _>("mark")
                .map_err(read("mark"))?
                .as_deref(),
            row.try_get("effective").map_err(read("effective"))?,
        )?,
    })
}

fn id(text: String, column: &'static str) -> Result<SessionId, TopicError> {
    SessionId::try_from(text).map_err(|error| stored(column)(Box::new(error)))
}
