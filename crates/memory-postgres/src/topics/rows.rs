use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::DateTime;
use chrono::Utc;
use kernel::owner::Owner;
use kernel::tenancy::SessionVisibility;
use kernel::tenancy::Visibility;
use kernel::topics::Alias;
use kernel::topics::PlacedSession;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Root;
use kernel::topics::RootKind;
use kernel::topics::Slug;
use kernel::topics::Suggestion;
use kernel::topics::Topic;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use sqlx::PgConnection;
use uuid::Uuid;
use wire::session_id::SessionId;

use crate::error::Error;

#[derive(sqlx::FromRow)]
pub(super) struct TopicRow {
    pub(super) id: Uuid,
    pub(super) kind: String,
    pub(super) slug: String,
    pub(super) name: String,
    pub(super) parent_id: Option<Uuid>,
    pub(super) directory_kind: Option<String>,
    pub(super) directory_node: Option<String>,
    pub(super) directory_path: Option<String>,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) closed_at: Option<DateTime<Utc>>,
    pub(super) owner_user_id: Option<String>,
    pub(super) visibility: String,
    pub(super) visibility_mark: Option<String>,
}

pub(super) const LISTING: &str =
    "SELECT t.id, t.kind, t.slug, t.name, t.parent_id, t.directory_kind,
       t.directory_node, t.directory_path, t.created_at, t.updated_at, t.closed_at, t.owner_user_id,
       t.visibility, t.visibility_mark,
       (SELECT count(*) FROM rh_session_placements p
         WHERE p.topic_id = t.id
           AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = p.session_id))
         AS session_count,
       (SELECT max(c.updated_at)
          FROM rh_session_placements p
          JOIN rh_conversations c ON c.session_key = p.session_id
         WHERE p.topic_id = t.id
           AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = p.session_id))
         AS last_active
  FROM rh_topics t";

pub(super) const PLACED: &str =
    "SELECT p.session_id, p.topic_id, p.placed_by, p.placed_at, p.owner_user_id,
       c.last_active
  FROM rh_session_placements p
  LEFT JOIN LATERAL (
      SELECT max(updated_at) AS last_active
        FROM rh_conversations
       WHERE session_key = p.session_id
  ) c ON true
 WHERE p.topic_id = $1
   AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = p.session_id)
 ORDER BY c.last_active DESC NULLS LAST, p.session_id";

#[derive(sqlx::FromRow)]
pub(super) struct ListingRow {
    #[sqlx(flatten)]
    pub(super) topic: TopicRow,
    pub(super) session_count: i64,
    pub(super) last_active: Option<DateTime<Utc>>,
}

pub(super) struct AliasRow {
    pub(super) topic_id: Uuid,
    pub(super) alias: String,
}

pub(super) struct PlacementRow {
    pub(super) session_id: String,
    pub(super) topic_id: Uuid,
    pub(super) placed_by: String,
    pub(super) placed_at: DateTime<Utc>,
    pub(super) owner_user_id: String,
}

pub(super) struct SuggestionRow {
    pub(super) session_id: String,
    pub(super) name: String,
    pub(super) topic_id: Option<Uuid>,
    pub(super) parent_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
pub(super) struct PlacedRow {
    pub(super) session_id: String,
    pub(super) topic_id: Uuid,
    pub(super) placed_by: String,
    pub(super) placed_at: DateTime<Utc>,
    pub(super) owner_user_id: String,
    pub(super) last_active: Option<DateTime<Utc>>,
}

pub(super) fn failed(operation: &'static str) -> impl FnOnce(sqlx::Error) -> TopicError {
    move |source| TopicError::Unavailable(Box::new(Error::operation(operation, source)))
}

pub(super) fn stored(
    field: &'static str,
) -> impl FnOnce(Box<dyn std::error::Error + Send + Sync>) -> TopicError {
    move |source| TopicError::Unavailable(Box::new(Error::StoredValue { field, source }))
}

pub(super) fn topic(row: TopicRow, aliases: Vec<Alias>) -> Result<Topic, TopicError> {
    let directory = match row.directory_kind {
        None => None,
        Some(kind) => {
            let kind = RootKind::from_str(&kind)
                .map_err(|error| stored("directory_kind")(Box::new(error)))?;
            let path = row
                .directory_path
                .ok_or_else(|| stored("directory_path")("a directory without a path".into()))?;
            Some(
                Root::from_parts(kind, row.directory_node, path).ok_or_else(|| {
                    stored("directory_node")("a directory node that does not match its kind".into())
                })?,
            )
        }
    };
    Ok(Topic {
        id: TopicId::from_uuid(row.id),
        kind: TopicKind::from_str(&row.kind).map_err(|error| stored("kind")(Box::new(error)))?,
        slug: Slug::fold(&row.slug)?,
        name: TopicName::new(&row.name)?,
        aliases,
        parent: row.parent_id.map(TopicId::from_uuid),
        directory,
        created_at: row.created_at,
        updated_at: row.updated_at,
        closed_at: row.closed_at,
        owner: row.owner_user_id.map(owner).transpose()?,
        visibility: visibility(&row.visibility)?,
        visibility_mark: row.visibility_mark.as_deref().map(visibility).transpose()?,
    })
}

pub(super) fn visibility(text: &str) -> Result<Visibility, TopicError> {
    Visibility::parse(text).map_err(|error| stored("visibility")(Box::new(error)))
}

pub(super) fn placement(row: PlacementRow) -> Result<Placement, TopicError> {
    Ok(Placement {
        session: SessionId::try_from(row.session_id)
            .map_err(|error| stored("session_id")(Box::new(error)))?,
        topic: TopicId::from_uuid(row.topic_id),
        by: PlacementSource::from_str(&row.placed_by)
            .map_err(|error| stored("placed_by")(Box::new(error)))?,
        at: row.placed_at,
        owner: Some(owner(row.owner_user_id)?),
    })
}

pub(super) fn owner(id: String) -> Result<Owner, TopicError> {
    Owner::new(id).map_err(|error| stored("owner_user_id")(Box::new(error)))
}

pub(super) fn session_visibility(
    owner: Option<String>,
    mark: Option<&str>,
    effective: Option<String>,
) -> Result<Option<SessionVisibility>, TopicError> {
    let parse =
        |text: &str| Visibility::parse(text).map_err(|error| stored("visibility")(Box::new(error)));
    effective
        .map(|effective| {
            Ok(SessionVisibility {
                owner: owner.map(self::owner).transpose()?,
                mark: mark.map(parse).transpose()?,
                effective: parse(&effective)?,
            })
        })
        .transpose()
}

pub(super) fn suggestion(row: SuggestionRow) -> Result<Suggestion, TopicError> {
    Ok(Suggestion {
        session: SessionId::try_from(row.session_id)
            .map_err(|error| stored("session_id")(Box::new(error)))?,
        name: TopicName::new(&row.name)?,
        topic: row.topic_id.map(TopicId::from_uuid),
        parent: row.parent_id.map(TopicId::from_uuid),
    })
}

pub(super) fn placed(row: PlacedRow) -> Result<PlacedSession, TopicError> {
    Ok(PlacedSession {
        last_active: row.last_active,
        placement: placement(PlacementRow {
            session_id: row.session_id,
            topic_id: row.topic_id,
            placed_by: row.placed_by,
            placed_at: row.placed_at,
            owner_user_id: row.owner_user_id,
        })?,
    })
}

pub(super) fn grouped(rows: Vec<AliasRow>) -> Result<BTreeMap<Uuid, Vec<Alias>>, TopicError> {
    let mut grouped: BTreeMap<Uuid, Vec<Alias>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.topic_id)
            .or_default()
            .push(Alias::new(&row.alias)?);
    }
    Ok(grouped)
}

pub(super) async fn aliases_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<Alias>, TopicError> {
    let rows = sqlx::query_as!(
        AliasRow,
        "SELECT topic_id, alias FROM rh_topic_aliases WHERE topic_id = $1 ORDER BY alias_key",
        id
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(failed("topic aliases"))?;
    rows.iter().map(|row| Alias::new(&row.alias)).collect()
}

pub(super) async fn visible(
    conn: &mut PgConnection,
    id: TopicId,
) -> Result<Option<Topic>, TopicError> {
    let row = sqlx::query_as!(
        TopicRow,
        "SELECT id, kind, slug, name, parent_id, directory_kind, directory_node, directory_path,
                created_at, updated_at, closed_at, owner_user_id, visibility, visibility_mark
           FROM rh_topics WHERE id = $1",
        id.uuid()
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed("topic read"))?;
    match row {
        Some(row) => {
            let aliases = aliases_of(conn, row.id).await?;
            topic(row, aliases).map(Some)
        }
        None => Ok(None),
    }
}

pub(super) async fn by_id(
    conn: &mut PgConnection,
    id: TopicId,
) -> Result<Option<Topic>, TopicError> {
    let row = sqlx::query_as!(
        TopicRow,
        "SELECT id, kind, slug, name, parent_id, directory_kind, directory_node, directory_path,
                created_at, updated_at, closed_at, owner_user_id, visibility, visibility_mark
           FROM rh_topics WHERE id = $1 FOR UPDATE",
        id.uuid()
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed("topic read"))?;
    match row {
        Some(row) => {
            let aliases = aliases_of(conn, row.id).await?;
            topic(row, aliases).map(Some)
        }
        None => Ok(None),
    }
}

pub(super) async fn by_slug(
    conn: &mut PgConnection,
    kind: TopicKind,
    slug: &Slug,
) -> Result<Option<Topic>, TopicError> {
    let row = sqlx::query_as!(
        TopicRow,
        "SELECT t.id, t.kind, t.slug, t.name, t.parent_id, t.directory_kind, t.directory_node,
                t.directory_path, t.created_at, t.updated_at, t.closed_at, t.owner_user_id,
                t.visibility, t.visibility_mark
           FROM rh_topics t
          WHERE t.kind = $2
            AND (t.slug = $1
                 OR t.id = (SELECT r.topic_id FROM rh_topic_redirects r WHERE r.slug = $1))
          ORDER BY (t.slug = $1) DESC
          LIMIT 1",
        slug.as_str(),
        kind.to_string()
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed("topic slug lookup"))?;
    match row {
        Some(row) => {
            let aliases = aliases_of(conn, row.id).await?;
            topic(row, aliases).map(Some)
        }
        None => Ok(None),
    }
}

pub(super) async fn placement_of(
    conn: &mut PgConnection,
    session: &SessionId,
) -> Result<Option<Placement>, TopicError> {
    sqlx::query_as!(
        PlacementRow,
        "SELECT session_id, topic_id, placed_by, placed_at, owner_user_id
           FROM rh_session_placements WHERE session_id = $1",
        session.as_str()
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed("session placement read"))?
    .map(placement)
    .transpose()
}

pub(super) fn not_found(id: TopicId) -> TopicError {
    TopicError::NotFound {
        key: id.to_string(),
    }
}
