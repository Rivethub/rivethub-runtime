use std::collections::BTreeMap;
use std::str::FromStr;

use kernel::owner::Owner;
use kernel::tenancy::Visibility;
use kernel::topics::Alias;
use kernel::topics::PlacedSession;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Redirect;
use kernel::topics::Root;
use kernel::topics::RootKind;
use kernel::topics::Slug;
use kernel::topics::Suggestion;
use kernel::topics::Topic;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::topics::TopicListing;
use kernel::topics::TopicName;
use kernel::topics::TopicTree;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use wire::session_id::SessionId;

use crate::error::SqliteError;
use crate::time;

const TOPIC_COLUMNS: &str = "t.id, t.slug, t.name, t.parent_id, t.directory_kind, \
     t.directory_node, t.directory_path, t.created_at, t.updated_at, t.closed_at, t.kind, \
     t.owner_user_id, t.visibility, t.visibility_mark";

pub(super) const PLACEMENT_COLUMNS: &str =
    "p.session_id, p.topic_id, p.placed_by, p.placed_at, p.owner_user_id";

pub(super) struct TopicRow {
    id: String,
    slug: String,
    name: String,
    parent_id: Option<String>,
    directory_kind: Option<String>,
    directory_node: Option<String>,
    directory_path: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    kind: String,
    owner_user_id: Option<String>,
    visibility: String,
    visibility_mark: Option<String>,
}

pub(super) struct PlacementRow {
    pub(super) session_id: String,
    pub(super) topic_id: String,
    pub(super) placed_by: String,
    pub(super) placed_at: String,
    pub(super) owner_user_id: Option<String>,
}

pub(super) type SuggestionColumns = (String, String, Option<String>, Option<String>);

pub(super) fn failed(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> TopicError {
    move |source| TopicError::Unavailable(Box::new(SqliteError::Query { operation, source }))
}

pub(super) fn stored(field: &'static str) -> impl FnOnce(kernel::BoxError) -> TopicError {
    move |source| {
        TopicError::Unavailable(Box::new(SqliteError::Invalid {
            message: format!("stored {field} is invalid: {source}"),
        }))
    }
}

pub(super) fn timestamp(
    text: &str,
    column: &'static str,
) -> Result<chrono::DateTime<chrono::Utc>, TopicError> {
    time::stored(text, column).map_err(|error| TopicError::Unavailable(Box::new(error)))
}

pub(super) fn topic_id(text: &str) -> Result<TopicId, TopicError> {
    TopicId::from_str(text).map_err(|error| stored("topic id")(Box::new(error)))
}

fn read_topic(row: &rusqlite::Row<'_>) -> rusqlite::Result<TopicRow> {
    Ok(TopicRow {
        id: row.get(0)?,
        slug: row.get(1)?,
        name: row.get(2)?,
        parent_id: row.get(3)?,
        directory_kind: row.get(4)?,
        directory_node: row.get(5)?,
        directory_path: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        closed_at: row.get(9)?,
        kind: row.get(10)?,
        owner_user_id: row.get(11)?,
        visibility: row.get(12)?,
        visibility_mark: row.get(13)?,
    })
}

pub(super) fn read_placement(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlacementRow> {
    Ok(PlacementRow {
        session_id: row.get(0)?,
        topic_id: row.get(1)?,
        placed_by: row.get(2)?,
        placed_at: row.get(3)?,
        owner_user_id: row.get(4)?,
    })
}

fn topic(row: TopicRow, aliases: Vec<Alias>) -> Result<Topic, TopicError> {
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
        id: topic_id(&row.id)?,
        kind: TopicKind::from_str(&row.kind).map_err(|error| stored("kind")(Box::new(error)))?,
        slug: Slug::fold(&row.slug)?,
        name: TopicName::new(&row.name)?,
        aliases,
        parent: row.parent_id.as_deref().map(topic_id).transpose()?,
        directory,
        created_at: timestamp(&row.created_at, "created_at")?,
        updated_at: timestamp(&row.updated_at, "updated_at")?,
        closed_at: row
            .closed_at
            .as_deref()
            .map(|text| timestamp(text, "closed_at"))
            .transpose()?,
        owner: row.owner_user_id.map(owner).transpose()?,
        visibility: visibility(&row.visibility)?,
        visibility_mark: row.visibility_mark.as_deref().map(visibility).transpose()?,
    })
}

fn visibility(text: &str) -> Result<Visibility, TopicError> {
    Visibility::parse(text).map_err(|error| stored("visibility")(Box::new(error)))
}

pub(super) fn placement(row: PlacementRow) -> Result<Placement, TopicError> {
    Ok(Placement {
        session: SessionId::try_from(row.session_id)
            .map_err(|error| stored("session_id")(Box::new(error)))?,
        topic: topic_id(&row.topic_id)?,
        by: PlacementSource::from_str(&row.placed_by)
            .map_err(|error| stored("placed_by")(Box::new(error)))?,
        at: timestamp(&row.placed_at, "placed_at")?,
        owner: row.owner_user_id.map(owner).transpose()?,
    })
}

pub(super) fn owner(id: String) -> Result<Owner, TopicError> {
    Owner::new(id).map_err(|error| stored("owner_user_id")(Box::new(error)))
}

fn aliases_of(conn: &Connection, id: &str) -> Result<Vec<Alias>, TopicError> {
    let texts = conn
        .prepare("SELECT alias FROM rh_topic_aliases WHERE topic_id = ?1 ORDER BY alias_key")
        .and_then(|mut statement| {
            statement
                .query_map(params![id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic aliases"))?;
    texts.iter().map(|text| Alias::new(text)).collect()
}

fn one(
    conn: &Connection,
    filter: &str,
    keys: &[&dyn rusqlite::ToSql],
) -> Result<Option<Topic>, TopicError> {
    let found = conn
        .query_row(
            &format!("SELECT {TOPIC_COLUMNS} FROM rh_topics t WHERE {filter} LIMIT 1"),
            keys,
            read_topic,
        )
        .optional()
        .map_err(failed("topic read"))?;
    match found {
        Some(row) => {
            let aliases = aliases_of(conn, &row.id)?;
            topic(row, aliases).map(Some)
        }
        None => Ok(None),
    }
}

pub(super) fn by_id(conn: &Connection, id: TopicId) -> Result<Option<Topic>, TopicError> {
    one(conn, "t.id = ?1", params![id.to_string()])
}

pub(super) fn by_slug(
    conn: &Connection,
    kind: TopicKind,
    slug: &Slug,
) -> Result<Option<Topic>, TopicError> {
    one(
        conn,
        "t.kind = ?2 AND (t.slug = ?1 \
         OR t.id = (SELECT r.topic_id FROM rh_topic_redirects r WHERE r.slug = ?1)) \
         ORDER BY (t.slug = ?1) DESC",
        params![slug.as_str(), kind.to_string()],
    )
}

pub(super) fn listings(conn: &Connection) -> Result<Vec<TopicListing>, TopicError> {
    let rows = conn
        .prepare(&format!(
            "SELECT {TOPIC_COLUMNS},
                    (SELECT count(*) FROM rh_session_placements p WHERE p.topic_id = t.id
                       AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = p.session_id)),
                    (SELECT max(c.updated_at)
                       FROM rh_session_placements p
                       JOIN ros_conversations c ON c.session_key = p.session_id
                      WHERE p.topic_id = t.id)
               FROM rh_topics t"
        ))
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        read_topic(row)?,
                        row.get::<_, i64>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic list"))?;
    let alias_rows = conn
        .prepare("SELECT topic_id, alias FROM rh_topic_aliases ORDER BY alias_key")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic list"))?;
    let mut aliases: BTreeMap<String, Vec<Alias>> = BTreeMap::new();
    for (owner, text) in alias_rows {
        aliases.entry(owner).or_default().push(Alias::new(&text)?);
    }
    rows.into_iter()
        .map(|(row, sessions, last_active)| {
            let own = aliases.remove(&row.id).into_iter().flatten().collect();
            Ok(TopicListing {
                topic: topic(row, own)?,
                session_count: u64::try_from(sessions).map_err(|error| {
                    TopicError::Unavailable(Box::new(SqliteError::Count(error)))
                })?,
                last_active: last_active
                    .as_deref()
                    .map(|text| timestamp(text, "updated_at"))
                    .transpose()?,
            })
        })
        .collect()
}

pub(super) fn redirects(conn: &Connection) -> Result<Vec<Redirect>, TopicError> {
    let rows = conn
        .prepare("SELECT slug, topic_id FROM rh_topic_redirects ORDER BY slug")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic redirects"))?;
    rows.into_iter()
        .map(|(slug, topic)| {
            Ok(Redirect {
                slug: Slug::fold(&slug)?,
                topic: topic_id(&topic)?,
            })
        })
        .collect()
}

pub(super) fn placement_of(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<Placement>, TopicError> {
    conn.query_row(
        &format!("SELECT {PLACEMENT_COLUMNS} FROM rh_session_placements p WHERE p.session_id = ?1"),
        params![session.as_str()],
        read_placement,
    )
    .optional()
    .map_err(failed("session placement read"))?
    .map(placement)
    .transpose()
}

pub(super) fn suggestion_of(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<Suggestion>, TopicError> {
    let row = conn
        .query_row(
            "SELECT session_id, name, topic_id, parent_id
               FROM rh_session_suggestions WHERE session_id = ?1",
            params![session.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(failed("session suggestion read"))?;
    row.map(suggestion).transpose()
}

pub(super) fn suggestion(
    (session_id, name, topic, parent): SuggestionColumns,
) -> Result<Suggestion, TopicError> {
    Ok(Suggestion {
        session: SessionId::try_from(session_id)
            .map_err(|error| stored("session_id")(Box::new(error)))?,
        name: TopicName::new(&name)?,
        topic: topic.as_deref().map(topic_id).transpose()?,
        parent: parent.as_deref().map(topic_id).transpose()?,
    })
}

pub(super) fn placed_in(conn: &Connection, id: TopicId) -> Result<Vec<PlacedSession>, TopicError> {
    let rows = conn
        .prepare(&format!(
            "SELECT {PLACEMENT_COLUMNS},
                    (SELECT max(c.updated_at) FROM ros_conversations c
                      WHERE c.session_key = p.session_id) AS last_active
               FROM rh_session_placements p
              WHERE p.topic_id = ?1
                AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = p.session_id)
              ORDER BY last_active DESC NULLS LAST, p.session_id"
        ))
        .and_then(|mut statement| {
            statement
                .query_map(params![id.to_string()], |row| {
                    Ok((read_placement(row)?, row.get::<_, Option<String>>(5)?))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic sessions"))?;
    rows.into_iter()
        .map(|(row, last_active)| {
            Ok(PlacedSession {
                placement: placement(row)?,
                last_active: last_active
                    .as_deref()
                    .map(|text| timestamp(text, "updated_at"))
                    .transpose()?,
            })
        })
        .collect()
}

pub(super) fn tree(conn: &Connection) -> Result<TopicTree, TopicError> {
    let edges = conn
        .prepare("SELECT id, parent_id, kind FROM rh_topics ORDER BY id")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("topic tree read"))?;
    let mut parents = Vec::with_capacity(edges.len());
    for (id, parent, kind) in &edges {
        parents.push((
            topic_id(id)?,
            parent.as_deref().map(topic_id).transpose()?,
            TopicKind::from_str(kind).map_err(|error| stored("kind")(Box::new(error)))?,
        ));
    }
    Ok(TopicTree::new(parents))
}

pub(super) fn subtopics(conn: &Connection, id: TopicId) -> Result<Vec<TopicId>, TopicError> {
    let ids = conn
        .prepare("SELECT id FROM rh_topics WHERE parent_id = ?1 ORDER BY id")
        .and_then(|mut statement| {
            statement
                .query_map(params![id.to_string()], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("sub-topic read"))?;
    ids.iter().map(|text| topic_id(text)).collect()
}

pub(super) fn not_found(id: TopicId) -> TopicError {
    TopicError::NotFound {
        key: id.to_string(),
    }
}
