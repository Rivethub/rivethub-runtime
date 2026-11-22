use jiff::Timestamp;
use kernel::wiki::entity_match::{TYPE_OBSERVATION_WINDOW, dominant_type};
use kernel::wiki::entity_pass::{
    EntityLeaf, EntityStoreError, TypeBrief, attribute_key, compact_key,
};
use rusqlite::{Connection, params};
use uuid::Uuid;

use super::{optional_uuid, query, rows, stamp, uuid};

#[derive(Debug, Clone)]
pub(super) struct TypeRow {
    pub(super) id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    pub(super) slug: String,
}

pub(super) fn load(conn: &Connection) -> Result<Vec<TypeRow>, EntityStoreError> {
    rows(
        conn,
        "SELECT id, parent_id, name, slug FROM rh_topics WHERE kind = 'type'",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        },
    )?
    .into_iter()
    .map(|(id, parent, name, slug)| {
        Ok(TypeRow {
            id: uuid(&id)?,
            parent_id: optional_uuid(parent.as_deref())?,
            name,
            slug,
        })
    })
    .collect()
}

pub(super) fn paths(conn: &Connection) -> Result<Vec<String>, EntityStoreError> {
    let rows = load(conn)?;
    let mut paths: Vec<String> = rows.iter().map(|row| type_path(row, &rows)).collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub(super) fn type_path(row: &TypeRow, rows: &[TypeRow]) -> String {
    let mut names = vec![row.name.clone()];
    let mut parent = row.parent_id;
    for _ in 0..16 {
        let Some(id) = parent else {
            break;
        };
        let Some(next) = rows.iter().find(|item| item.id == id) else {
            break;
        };
        names.push(next.name.clone());
        parent = next.parent_id;
    }
    names.reverse();
    names.join(" › ")
}

fn spelled_like<'a>(wanted: &str, rows: &'a [TypeRow]) -> Vec<&'a TypeRow> {
    let key = compact_key(wanted);
    if key.is_empty() {
        return Vec::new();
    }
    rows.iter()
        .filter(|row| {
            plural_of(&key, &compact_key(&row.name)) || plural_of(&key, &compact_key(&row.slug))
        })
        .collect()
}

fn plural_of(left: &str, right: &str) -> bool {
    let (short, long) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    long.strip_prefix(short)
        .is_some_and(|rest| rest == "s" || rest == "es")
}

pub(super) fn match_type<'a>(wanted: &str, rows: &'a [TypeRow]) -> Option<&'a TypeRow> {
    let trimmed = wanted.trim();
    if trimmed.is_empty() {
        return None;
    }
    let folded = compact_key(trimmed);
    rows.iter().find(|row| {
        let path = type_path(row, rows);
        row.name.eq_ignore_ascii_case(trimmed)
            || row.slug.eq_ignore_ascii_case(trimmed)
            || path.eq_ignore_ascii_case(trimmed)
            || compact_key(&row.name) == folded
            || compact_key(&row.slug) == folded
            || compact_key(&path) == folded
    })
}

pub(super) fn ensure(
    conn: &Connection,
    wanted: &str,
    rows: &mut Vec<TypeRow>,
    owner: &str,
    now: Timestamp,
) -> Result<Option<TypeRow>, EntityStoreError> {
    if let Some(found) = match_type(wanted, rows) {
        return Ok(Some(found.clone()));
    }
    let spelled = spelled_like(wanted, rows);
    if let [only] = spelled.as_slice() {
        return Ok(Some((*only).clone()));
    }
    let name = wanted.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Ok(None);
    }
    let Some(slug) = kernel::topics::Slug::of_heading(name) else {
        return Ok(None);
    };
    let slug = slug.as_str().to_owned();
    if let Some(found) = rows.iter().find(|row| row.slug == slug) {
        return Ok(Some(found.clone()));
    }
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id)
         VALUES (?1, 'type', ?2, ?3, ?4, ?4, ?5)",
        params![id.to_string(), slug, name, stamp(now), owner],
    )
    .map_err(query)?;
    let row = TypeRow {
        id,
        parent_id: None,
        name: name.to_owned(),
        slug,
    };
    rows.push(row.clone());
    Ok(Some(row))
}

pub(super) fn briefs(conn: &Connection) -> Result<Vec<TypeBrief>, EntityStoreError> {
    let types = load(conn)?;
    let aliases = rows(
        conn,
        "SELECT topic_id, alias FROM rh_topic_aliases WHERE kind = 'type' ORDER BY alias_key",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let examples = rows(
        conn,
        "SELECT parent_id, name FROM (
            SELECT parent_id, name, row_number() OVER (
                PARTITION BY parent_id ORDER BY rh_instant(updated_at) DESC, id
            ) AS position FROM rh_topics WHERE kind = 'entity' AND parent_id IS NOT NULL
         ) WHERE position <= 3 ORDER BY parent_id, position",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut briefs: Vec<_> = types
        .iter()
        .map(|row| {
            let id = row.id.to_string();
            TypeBrief {
                name: type_path(row, &types),
                slug: row.slug.clone(),
                aliases: for_type(&aliases, &id),
                examples: for_type(&examples, &id),
            }
        })
        .collect();
    briefs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(briefs)
}

fn for_type(pairs: &[(String, String)], id: &str) -> Vec<String> {
    pairs
        .iter()
        .filter(|(parent, _)| parent == id)
        .map(|(_, name)| name.clone())
        .collect()
}

pub(super) fn remember_alias(
    conn: &Connection,
    type_name: &str,
    alias: &str,
) -> Result<(), EntityStoreError> {
    let alias = alias.trim();
    let Some(key) = attribute_key(alias) else {
        return Ok(());
    };
    let rows = load(conn)?;
    let Some(row) = rows.iter().find(|row| {
        type_path(row, &rows) == type_name || row.name == type_name || row.slug == type_name
    }) else {
        return Ok(());
    };
    let folded = compact_key(alias);
    if folded == compact_key(&row.name) || folded == compact_key(&row.slug) {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
         VALUES (?1, ?2, ?3, 'type') ON CONFLICT (kind, alias_key) DO NOTHING",
        params![row.id.to_string(), alias, key],
    )
    .map_err(query)?;
    Ok(())
}

pub(super) fn observed(conn: &Connection, id: Uuid) -> Result<Vec<String>, EntityStoreError> {
    let window = i64::try_from(TYPE_OBSERVATION_WINDOW).map_err(super::error)?;
    rows(
        conn,
        "SELECT type_name FROM rh_entity_type_seen WHERE entity_id = ?1
         ORDER BY rh_instant(observed_at) DESC, summary_id DESC, type_name DESC LIMIT ?2",
        params![id.to_string(), window],
        |row| row.get(0),
    )
}

pub(super) fn observe(
    conn: &Connection,
    entity: Uuid,
    leaf: &EntityLeaf,
    type_name: &str,
    owner: &str,
    types: &[TypeRow],
) -> Result<(), EntityStoreError> {
    let name = type_name.trim();
    if !name.is_empty() {
        conn.execute(
            "INSERT INTO rh_entity_type_seen (entity_id, summary_id, type_name, observed_at, owner_user_id)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
            params![entity.to_string(), leaf.id.to_string(), name, stamp(leaf.start), owner],
        ).map_err(query)?;
    }
    let observed = observed(conn, entity)?;
    if let Some(kind) = dominant_type(&observed).and_then(|name| match_type(name, types)) {
        conn.execute(
            "UPDATE rh_topics SET parent_id = ?2, updated_at = ?3
             WHERE id = ?1 AND kind = 'entity' AND parent_id IS NOT ?2",
            params![
                entity.to_string(),
                kind.id.to_string(),
                stamp(Timestamp::now())
            ],
        )
        .map_err(query)?;
    }
    Ok(())
}
