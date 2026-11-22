use jiff::Timestamp;
use kernel::wiki::entity_match::host_party_mismatch;
use kernel::wiki::entity_pass::{EntityMerge, EntityStoreError};
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use uuid::Uuid;

use super::{at, identity, pages, query, rows, stamp};

struct Side {
    id: Uuid,
    name: String,
    slug: String,
    kind: String,
    type_name: String,
    parent: Option<String>,
    created: Timestamp,
}

struct StoredSide {
    name: String,
    slug: String,
    kind: String,
    type_name: String,
    parent: Option<String>,
    created: String,
}

pub(super) fn pair(
    conn: &Connection,
    left: Uuid,
    right: Uuid,
    now: Timestamp,
) -> Result<Option<EntityMerge>, EntityStoreError> {
    match (load(conn, left)?, load(conn, right)?) {
        (Some(left), Some(right)) => both(conn, left, right, now),
        (Some(found), None) => Ok(already(found, right)),
        (None, Some(found)) => Ok(already(found, left)),
        (None, None) => Ok(None),
    }
}

fn load(conn: &Connection, id: Uuid) -> Result<Option<Side>, EntityStoreError> {
    let row: Option<StoredSide> = conn
        .query_row(
            "SELECT t.name, t.slug, t.kind, coalesce(p.name, ''), t.parent_id, t.created_at
         FROM rh_topics t LEFT JOIN rh_topics p ON p.id = t.parent_id WHERE t.id = ?1",
            [id.to_string()],
            read_side,
        )
        .optional()
        .map_err(query)?;
    row.map(|row| {
        Ok(Side {
            id,
            name: row.name,
            slug: row.slug,
            kind: row.kind,
            type_name: row.type_name,
            parent: row.parent,
            created: at(&row.created)?,
        })
    })
    .transpose()
}

fn read_side(row: &Row<'_>) -> rusqlite::Result<StoredSide> {
    Ok(StoredSide {
        name: row.get(0)?,
        slug: row.get(1)?,
        kind: row.get(2)?,
        type_name: row.get(3)?,
        parent: row.get(4)?,
        created: row.get(5)?,
    })
}

fn already(found: Side, missing: Uuid) -> Option<EntityMerge> {
    (found.kind == "entity").then_some(EntityMerge {
        survivor: found.id,
        absorbed: missing,
        survivor_name: found.name,
        absorbed_name: String::new(),
        performed: false,
    })
}

fn both(
    conn: &Connection,
    left: Side,
    right: Side,
    now: Timestamp,
) -> Result<Option<EntityMerge>, EntityStoreError> {
    if left.kind != "entity"
        || right.kind != "entity"
        || host_party_mismatch(&left.type_name, &right.type_name)
    {
        return Ok(None);
    }
    let (survivor, absorbed) = if (left.created, left.id) <= (right.created, right.id) {
        (left, right)
    } else {
        (right, left)
    };
    let keep = survivor.id.to_string();
    let drop = absorbed.id.to_string();
    move_facts(conn, &keep, &drop)?;
    move_evidence(conn, &keep, &drop)?;
    conn.execute(
        "UPDATE rh_topic_aliases SET topic_id = ?1 WHERE topic_id = ?2",
        params![keep, drop],
    )
    .map_err(query)?;
    identity::add_alias(conn, survivor.id, &absorbed.name)?;
    redirect_wiki(conn, &survivor, &absorbed, now)?;
    repoint(conn, &keep, &drop, absorbed.parent.as_deref())?;
    retire(conn, &survivor, &absorbed, now)?;
    Ok(Some(EntityMerge {
        survivor: survivor.id,
        absorbed: absorbed.id,
        survivor_name: survivor.name,
        absorbed_name: absorbed.name,
        performed: true,
    }))
}

fn move_facts(conn: &Connection, keep: &str, drop: &str) -> Result<(), EntityStoreError> {
    let facts = rows(
        conn,
        "SELECT id, key FROM rh_entity_facts WHERE topic_id = ?1 ORDER BY rh_instant(created_at), id",
        [drop],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    for (fact, key) in facts {
        let kept: Option<String> = conn
            .query_row(
                "SELECT id FROM rh_entity_facts WHERE topic_id = ?1 AND key = ?2
             ORDER BY rh_instant(created_at), id LIMIT 1",
                params![keep, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(query)?;
        if let Some(kept) = kept {
            fold_fact(conn, &kept, &fact)?;
        } else {
            conn.execute(
                "UPDATE rh_entity_facts SET topic_id = ?1 WHERE id = ?2",
                params![keep, fact],
            )
            .map_err(query)?;
        }
    }
    Ok(())
}

fn fold_fact(conn: &Connection, keep: &str, drop: &str) -> Result<(), EntityStoreError> {
    let bound = params![keep, drop];
    conn.execute(
        "DELETE FROM rh_entity_fact_claims WHERE id IN (
            SELECT id FROM (
                SELECT id, row_number() OVER (
                    PARTITION BY summary_id ORDER BY rh_instant(at) DESC, rh_instant(recorded_at) DESC, id DESC
                ) AS position
                FROM rh_entity_fact_claims WHERE fact_id IN (?1, ?2) AND summary_id IS NOT NULL
            ) WHERE position > 1
         )", bound,
    ).map_err(query)?;
    conn.execute(
        "UPDATE rh_entity_fact_claims SET fact_id = ?1 WHERE fact_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute("DELETE FROM rh_entity_facts WHERE id = ?1", [drop])
        .map_err(query)?;
    Ok(())
}

fn move_evidence(conn: &Connection, keep: &str, drop: &str) -> Result<(), EntityStoreError> {
    let bound = params![keep, drop];
    conn.execute(
        "INSERT INTO rh_entity_history (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
         SELECT ?1, summary_id, conversation_id, session_on, sentence, owner_user_id
            FROM rh_entity_history WHERE entity_id = ?2 ON CONFLICT DO NOTHING", bound,
    ).map_err(query)?;
    conn.execute(
        "INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
         SELECT ?1, summary_id, type_name, observed_at, owner_user_id
            FROM rh_entity_type_seen WHERE entity_id = ?2 ON CONFLICT DO NOTHING", bound,
    ).map_err(query)?;
    conn.execute(
        "INSERT INTO rh_entity_applied (entity_id, applied_through, summary_id, owner_user_id)
         SELECT ?1, applied_through, summary_id, owner_user_id FROM rh_entity_applied WHERE entity_id = ?2
         ON CONFLICT (entity_id) DO UPDATE SET applied_through = excluded.applied_through,
            summary_id = excluded.summary_id, owner_user_id = excluded.owner_user_id
         WHERE (rh_instant(rh_entity_applied.applied_through), rh_entity_applied.summary_id)
             < (rh_instant(excluded.applied_through), excluded.summary_id)", bound,
    ).map_err(query)?;
    conn.execute(
        "INSERT INTO rh_entity_embeddings (entity_id, embedding, embedded_at, owner_user_id)
         SELECT ?1, embedding, embedded_at, owner_user_id FROM rh_entity_embeddings WHERE entity_id = ?2
         ON CONFLICT (entity_id) DO UPDATE SET embedding = excluded.embedding,
            embedded_at = excluded.embedded_at, owner_user_id = excluded.owner_user_id
         WHERE rh_instant(excluded.embedded_at) >= rh_instant(rh_entity_embeddings.embedded_at)", bound,
    ).map_err(query)?;
    Ok(())
}

fn redirect_wiki(
    conn: &Connection,
    keep: &Side,
    drop: &Side,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    if keep.slug == drop.slug {
        return Ok(());
    }
    let time = stamp(now);
    conn.execute(
        "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3) ON CONFLICT (slug) DO NOTHING",
        params![keep.slug, keep.name, time],
    )
    .map_err(query)?;
    pages::bind(conn, keep.id, &keep.slug)?;
    let bound = params![keep.slug, drop.slug];
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         SELECT ?1, summary_id, kind, note, cited_at FROM ros_wiki_citations WHERE topic_slug = ?2
         ON CONFLICT DO NOTHING",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at)
         SELECT ?1, source_kind, source_id, conversation_id, git_sha, created_at
         FROM ros_wiki_provenance WHERE topic_slug = ?2 ON CONFLICT DO NOTHING", bound,
    ).map_err(query)?;
    conn.execute(
        "UPDATE ros_wiki_redirects SET to_slug = ?1 WHERE to_slug = ?2 AND from_slug <> ?1",
        bound,
    )
    .map_err(query)?;
    conn.execute("DELETE FROM ros_wiki_topics WHERE slug = ?1", [&drop.slug])
        .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at) VALUES (?2, ?1, ?3)
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = excluded.to_slug, created_at = excluded.created_at",
        params![keep.slug, drop.slug, time],
    ).map_err(query)?;
    Ok(())
}

fn repoint(
    conn: &Connection,
    keep: &str,
    drop: &str,
    drop_parent: Option<&str>,
) -> Result<(), EntityStoreError> {
    let parent = drop_parent.filter(|id| *id != keep && *id != drop);
    conn.execute(
        "UPDATE rh_topics SET parent_id = ?3 WHERE id = ?1 AND parent_id = ?2",
        params![keep, drop, parent],
    )
    .map_err(query)?;
    let bound = params![keep, drop];
    conn.execute(
        "UPDATE rh_topics SET parent_id = ?1 WHERE parent_id = ?2 AND id NOT IN (?1, ?2)",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_topic_redirects SET topic_id = ?1 WHERE topic_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_session_placements SET topic_id = ?1 WHERE topic_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_conversation_placements SET topic_id = ?1 WHERE topic_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_session_suggestions SET topic_id = ?1 WHERE topic_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_session_suggestions SET parent_id = ?1 WHERE parent_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_topic_decisions SET topic_id = ?1 WHERE topic_id = ?2",
        bound,
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE rh_entity_decisions SET entity_id = ?1 WHERE entity_id = ?2",
        bound,
    )
    .map_err(query)?;
    Ok(())
}

fn retire(
    conn: &Connection,
    keep: &Side,
    drop: &Side,
    time: Timestamp,
) -> Result<(), EntityStoreError> {
    let at = stamp(time);
    let survivor = keep.id.to_string();
    conn.execute(
        "INSERT INTO rh_topic_tombstones (kind, slug, deleted_at, owner_user_id, locally_deleted)
         SELECT 'entity', ?1, ?2, owner_user_id, owner_user_id IS NULL FROM rh_topics WHERE id = ?3
         ON CONFLICT (kind, slug) DO UPDATE SET deleted_at = excluded.deleted_at,
             owner_user_id = excluded.owner_user_id, locally_deleted = excluded.locally_deleted",
        params![drop.slug, at, drop.id.to_string()],
    )
    .map_err(query)?;
    conn.execute("DELETE FROM rh_topics WHERE id = ?1", [drop.id.to_string()])
        .map_err(query)?;
    conn.execute(
        "INSERT INTO rh_topic_redirects (slug, topic_id, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (slug) DO UPDATE SET topic_id = excluded.topic_id, created_at = excluded.created_at",
        params![drop.slug, survivor, at],
    ).map_err(query)?;
    conn.execute(
        "UPDATE rh_topics SET updated_at = ?2 WHERE id = ?1",
        params![survivor, at],
    )
    .map_err(query)?;
    Ok(())
}
