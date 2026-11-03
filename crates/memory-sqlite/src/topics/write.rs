use kernel::filing::conversation_keys;
use kernel::owner::Owner;
use kernel::topics::MergePlan;
use kernel::topics::Placement;
use kernel::topics::Root;
use kernel::topics::Slug;
use kernel::topics::Suggestion;
use kernel::topics::SyncTopicPage;
use kernel::topics::Topic;
use kernel::topics::TopicError;
use kernel::topics::TopicId;
use kernel::wiki::page::WikiSlug;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

use super::rows::failed;
use crate::time;

pub(super) fn claim_names(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    let id = topic.id.to_string();
    let slug_taken = conn
        .query_row(
            "SELECT 1 FROM rh_topics WHERE kind = ?3 AND slug = ?1 AND id <> ?2",
            params![topic.slug.as_str(), id, topic.kind.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(failed("topic slug check"))?;
    if slug_taken.is_some() {
        return Err(TopicError::SlugTaken {
            slug: topic.slug.clone(),
            name: topic.name.as_str().to_owned(),
        });
    }
    for alias in &topic.aliases {
        let owner = conn
            .query_row(
                "SELECT alias FROM rh_topic_aliases
                  WHERE kind = ?3 AND alias_key = ?1 AND topic_id <> ?2",
                params![alias.key(), id, topic.kind.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(failed("topic alias check"))?;
        if let Some(alias) = owner {
            return Err(TopicError::AliasTaken { alias });
        }
    }
    Ok(())
}

pub(super) fn insert(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    let directory = topic.directory.as_ref();
    conn.execute(
        "INSERT INTO rh_topics (id, slug, name, parent_id, directory_kind, directory_node,
                                directory_path, created_at, updated_at, closed_at, kind,
                                owner_user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            topic.id.to_string(),
            topic.slug.as_str(),
            topic.name.as_str(),
            topic.parent.map(|parent| parent.to_string()),
            directory.map(|root| root.kind().to_string()),
            directory.and_then(Root::node),
            directory.map(Root::path),
            time::stamp(topic.created_at),
            time::stamp(topic.updated_at),
            topic.closed_at.map(time::stamp),
            topic.kind.to_string(),
            topic.owner.as_ref().map(Owner::as_str),
        ],
    )
    .map_err(failed("topic create"))?;
    Ok(())
}

pub(super) fn closure(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    conn.execute(
        "UPDATE rh_topics SET closed_at = ?2, updated_at = ?3 WHERE id = ?1",
        params![
            topic.id.to_string(),
            topic.closed_at.map(time::stamp),
            time::stamp(topic.updated_at),
        ],
    )
    .map_err(failed("topic close"))?;
    Ok(())
}

pub(super) fn row(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    let directory = topic.directory.as_ref();
    conn.execute(
        "UPDATE rh_topics
            SET slug = ?2, name = ?3, parent_id = ?4, directory_kind = ?5, directory_node = ?6,
                directory_path = ?7, updated_at = ?8
          WHERE id = ?1",
        params![
            topic.id.to_string(),
            topic.slug.as_str(),
            topic.name.as_str(),
            topic.parent.map(|parent| parent.to_string()),
            directory.map(|root| root.kind().to_string()),
            directory.and_then(Root::node),
            directory.map(Root::path),
            time::stamp(topic.updated_at),
        ],
    )
    .map_err(failed("topic update"))?;
    Ok(())
}

pub(super) fn aliases(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    let id = topic.id.to_string();
    conn.execute(
        "DELETE FROM rh_topic_aliases WHERE topic_id = ?1",
        params![id],
    )
    .map_err(failed("topic aliases"))?;
    for alias in &topic.aliases {
        conn.execute(
            "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, alias.as_str(), alias.key(), topic.kind.to_string()],
        )
        .map_err(failed("topic aliases"))?;
    }
    Ok(())
}

pub(super) fn fold_into(
    conn: &Connection,
    plan: &MergePlan,
    merged: &Topic,
) -> Result<(), TopicError> {
    let source = plan.source.to_string();
    let into = plan.into.to_string();
    let now = time::stamp(merged.updated_at);
    conn.execute(
        "UPDATE rh_session_placements SET topic_id = ?2 WHERE topic_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge placements"))?;
    conn.execute(
        "UPDATE rh_topic_redirects SET topic_id = ?2 WHERE topic_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge redirects"))?;
    conn.execute(
        "UPDATE rh_session_suggestions SET topic_id = ?2 WHERE topic_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge suggestions"))?;
    super::filing::fold_into(conn, &source, &into)?;
    conn.execute(
        "UPDATE rh_topic_decisions SET topic_id = ?2 WHERE topic_id = ?1",
        params![source, into],
    )
    .map_err(failed("topic merge decisions"))?;
    conn.execute(
        "DELETE FROM rh_topic_aliases WHERE topic_id = ?1",
        params![source],
    )
    .map_err(failed("topic merge aliases"))?;
    for subtopic in &plan.subtopics {
        conn.execute(
            "UPDATE rh_topics SET parent_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![subtopic.to_string(), into, now],
        )
        .map_err(failed("topic merge sub-topics"))?;
    }
    row(conn, merged)?;
    aliases(conn, merged)?;
    conn.execute("DELETE FROM rh_topics WHERE id = ?1", params![source])
        .map_err(failed("topic merge delete"))?;
    conn.execute(
        "INSERT INTO rh_topic_redirects (slug, topic_id, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (slug) DO UPDATE SET topic_id = excluded.topic_id,
                                          created_at = excluded.created_at",
        params![
            plan.redirect.slug.as_str(),
            plan.redirect.topic.to_string(),
            now
        ],
    )
    .map_err(failed("topic merge redirect"))?;
    Ok(())
}

pub(super) fn remove(conn: &Connection, topic: &Topic) -> Result<(), TopicError> {
    conn.execute(
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at)
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = excluded.deleted_at",
        params![topic.kind.to_string(), topic.slug.as_str()],
    )
    .map_err(failed("topic tombstone"))?;
    let id = topic.id.to_string();
    for (statement, operation) in [
        (
            "DELETE FROM rh_session_placements WHERE topic_id = ?1",
            "topic delete placements",
        ),
        (
            "DELETE FROM rh_session_suggestions WHERE topic_id = ?1",
            "topic delete suggestions",
        ),
        (
            "DELETE FROM rh_session_startup WHERE topic_id = ?1",
            "topic delete startup",
        ),
        (
            "DELETE FROM rh_topic_decisions WHERE topic_id = ?1",
            "topic delete decisions",
        ),
        ("DELETE FROM rh_topics WHERE id = ?1", "topic delete"),
    ] {
        conn.execute(statement, params![id])
            .map_err(failed(operation))?;
    }
    Ok(())
}

pub(super) fn rename_redirect(
    conn: &Connection,
    old: &Slug,
    renamed: &Topic,
) -> Result<(), TopicError> {
    conn.execute(
        "DELETE FROM rh_topic_redirects WHERE slug = ?1",
        params![renamed.slug.as_str()],
    )
    .map_err(failed("topic rename redirect"))?;
    conn.execute(
        "INSERT INTO rh_topic_redirects (slug, topic_id, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (slug) DO UPDATE SET topic_id = excluded.topic_id,
                                          created_at = excluded.created_at",
        params![
            old.as_str(),
            renamed.id.to_string(),
            time::stamp(renamed.updated_at)
        ],
    )
    .map_err(failed("topic rename redirect"))?;
    Ok(())
}

pub(super) fn placement(conn: &Connection, placement: &Placement) -> Result<(), TopicError> {
    conn.execute(
        "INSERT INTO rh_session_placements (session_id, topic_id, placed_by, placed_at, owner_user_id)
         VALUES (?1, ?2, ?3, ?4,
                 coalesce((SELECT c.owner_user_id FROM ros_conversations c
                            WHERE c.session_key IN (?1, ?6)
                            ORDER BY c.created_at LIMIT 1), ?5))
         ON CONFLICT (session_id) DO UPDATE SET topic_id = excluded.topic_id,
                                                placed_by = excluded.placed_by,
                                                placed_at = excluded.placed_at,
                                                owner_user_id = excluded.owner_user_id",
        params![
            placement.session.as_str(),
            placement.topic.to_string(),
            placement.by.to_string(),
            time::stamp(placement.at),
            placement.owner.as_ref().map(Owner::as_str),
            conversation_keys(&placement.session).get(1),
        ],
    )
    .map_err(failed("session placement write"))?;
    conn.execute(
        "DELETE FROM rh_session_suggestions WHERE session_id = ?1",
        params![placement.session.as_str()],
    )
    .map_err(failed("session suggestion clear"))?;
    Ok(())
}

pub(super) fn suggestion(conn: &Connection, suggestion: &Suggestion) -> Result<(), TopicError> {
    conn.execute(
        "INSERT INTO rh_session_suggestions (session_id, name, topic_id, parent_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (session_id) DO UPDATE SET name = excluded.name,
                                                topic_id = excluded.topic_id,
                                                parent_id = excluded.parent_id",
        params![
            suggestion.session.as_str(),
            suggestion.name.as_str(),
            suggestion.topic.map(|topic| topic.to_string()),
            suggestion.parent.map(|topic| topic.to_string()),
        ],
    )
    .map_err(failed("session suggestion write"))?;
    Ok(())
}

pub(super) fn page_of(conn: &Connection, topic: TopicId) -> Result<Option<WikiSlug>, TopicError> {
    conn.query_row(
        "SELECT page_slug FROM rh_topic_pages WHERE topic_id = ?1",
        params![topic.to_string()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(failed("topic page"))?
    .map(WikiSlug::try_from)
    .transpose()
    .map_err(|source| TopicError::Unavailable(Box::new(source)))
}

pub(super) fn enqueue_syncs(
    conn: &Connection,
    syncs: Vec<SyncTopicPage>,
) -> Result<(), TopicError> {
    let now = chrono::Utc::now();
    for sync in syncs {
        let job = sync.job().map_err(unavailable)?;
        let payload = serde_json::to_string(&job.payload).map_err(unavailable)?;
        let insert = crate::port::job_insert(&job, payload, now).map_err(unavailable)?;
        crate::queue::enqueue_in(conn, &insert).map_err(unavailable)?;
    }
    Ok(())
}

fn unavailable(source: impl std::error::Error + Send + Sync + 'static) -> TopicError {
    TopicError::Unavailable(Box::new(source))
}
