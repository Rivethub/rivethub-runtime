use jiff::Timestamp;
use kernel::memory::summary_reference::SummaryReferenceError;
use kernel::wiki::entity_match::{
    canonical_key, cross_names_match, identity_keys, names_same_entity, types_compatible,
};
use kernel::wiki::entity_pass::{
    BindRequest, EntityLeaf, EntityStoreError, EntityView, PreparedEntity, SlugOccupant,
    attribute_key, choose_slug, pick_existing,
};
use rusqlite::{Connection, Row, Transaction, params};
use uuid::Uuid;

use super::{at, claims, fence, pages, query, rows, stamp, types, uuid, work};
use crate::summary_replace;

pub(super) struct EntityRow {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) slug: String,
    pub(super) type_name: Option<String>,
    pub(super) created: Timestamp,
    pub(super) aliases: Vec<Alias>,
}

pub(super) struct Alias {
    pub(super) name: String,
    pub(super) key: String,
}

struct RawEntity {
    id: String,
    name: String,
    slug: String,
    created: String,
    type_name: Option<String>,
}

impl RawEntity {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            slug: row.get(2)?,
            created: row.get(3)?,
            type_name: row.get(4)?,
        })
    }
}

pub(super) fn entities(conn: &Connection) -> Result<Vec<EntityRow>, EntityStoreError> {
    let mut aliases = std::collections::HashMap::<String, Vec<Alias>>::new();
    for (id, name, key) in rows(
        conn,
        "SELECT topic_id, alias, alias_key FROM rh_topic_aliases WHERE kind = 'entity' ORDER BY alias_key",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )? {
        aliases.entry(id).or_default().push(Alias { name, key });
    }
    rows(
        conn,
        "SELECT t.id, t.name, t.slug, t.created_at, p.name FROM rh_topics t
         LEFT JOIN rh_topics p ON p.id = t.parent_id WHERE t.kind = 'entity' ORDER BY t.id",
        [],
        RawEntity::read,
    )?
    .into_iter()
    .map(|row| {
        Ok(EntityRow {
            id: uuid(&row.id)?,
            name: row.name,
            slug: row.slug,
            type_name: row.type_name,
            created: at(&row.created)?,
            aliases: aliases.remove(&row.id).unwrap_or_default(),
        })
    })
    .collect()
}

pub(super) fn raw_name(name: &str) -> String {
    name.chars()
        .filter_map(|ch| ch.to_lowercase().next())
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

pub(super) fn raw_alias(key: &str) -> String {
    key.to_lowercase().replace('-', "")
}

pub(super) fn folded(raw: &str) -> &str {
    raw.strip_suffix("ql")
        .filter(|_| raw.chars().count() >= 8)
        .unwrap_or(raw)
}

pub(super) fn exact_name(row: &EntityRow, keys: &[String]) -> bool {
    let matches = |key: &str| keys.iter().any(|wanted| wanted == folded(key));
    matches(&raw_name(&row.name))
        || row.name.split_once('/').is_some_and(|(_, tail)| {
            let tail = raw_name(tail);
            folded(&tail).len() >= 6 && matches(&tail)
        })
        || row
            .aliases
            .iter()
            .any(|alias| matches(&raw_alias(&alias.key)))
}

pub(super) fn commit(
    tx: &Transaction<'_>,
    leaves: &[EntityLeaf],
    prepared: &PreparedEntity,
    owner: &str,
    now: Timestamp,
) -> Result<Option<EntityView>, EntityStoreError> {
    for leaf in leaves {
        let reference = summary_replace::shared(tx, leaf.id).map_err(fence)?;
        if reference.owner.as_deref() != Some(owner) {
            return Err(SummaryReferenceError::Stale(leaf.id).into());
        }
        work::validate_leaf(tx, &reference, leaf)?;
    }
    work::guard(
        tx,
        prepared
            .facts
            .iter()
            .map(|fact| fact.summary_id)
            .chain(prepared.history.iter().map(|line| line.summary_id)),
        owner,
    )?;
    let mut types = types::load(tx)?;
    let Some(kind) = types::ensure(tx, &prepared.type_name, &mut types, owner, now)? else {
        return Ok(None);
    };
    let entities = entities(tx)?;
    let started = leaves.first().map_or(now, |leaf| leaf.start);
    let id = match resolve(prepared, &entities, &kind) {
        Some(id) => id,
        None => insert(tx, prepared, &kind, owner, started, now)?,
    };
    if let Some(leaf) = leaves.first() {
        types::observe(tx, id, leaf, &prepared.type_name, owner, &types)?;
    }
    write_aliases(tx, id, prepared, &entities)?;
    claims::assert_snapshot(tx, id, &prepared.facts, "", owner)?;
    for line in &prepared.history {
        pages::write_history(tx, id, line, owner, false)?;
    }
    pages::view(tx, id).map(Some)
}

fn resolve(
    prepared: &PreparedEntity,
    entities: &[EntityRow],
    kind: &types::TypeRow,
) -> Option<Uuid> {
    let keys = identity_keys(&prepared.name);
    if let Some(row) = entities
        .iter()
        .filter(|row| {
            row.type_name
                .as_deref()
                .is_some_and(|ty| names_same_entity(&prepared.type_name, ty))
                && (exact_name(row, &keys)
                    || keys.iter().any(|key| key == folded(&raw_name(&row.slug))))
        })
        .min_by_key(|row| (row.created, row.id))
    {
        return Some(row.id);
    }
    let ref_exists = entities.iter().any(|row| {
        Some(row.id) == prepared.entity_ref
            && row.type_name.as_deref().is_some_and(|ty| {
                types_compatible(&prepared.type_name, ty)
                    || cross_names_match(&prepared.type_name, &prepared.name, ty, &row.name)
            })
    });
    pick_existing(
        &BindRequest {
            entity_ref: prepared.entity_ref,
            ref_exists,
            type_id: Some(kind.id),
        },
        &[],
    )
}

fn insert(
    conn: &Connection,
    prepared: &PreparedEntity,
    kind: &types::TypeRow,
    owner: &str,
    started: Timestamp,
    now: Timestamp,
) -> Result<Uuid, EntityStoreError> {
    let base = attribute_key(&prepared.name).unwrap_or_else(|| kind.slug.clone());
    let candidates = serde_json::to_string(&[
        base.clone(),
        format!("{base}-{}", kind.slug),
        format!("{}-{base}", kind.slug),
    ])
    .map_err(super::error)?;
    let occupied: Vec<String> = rows(
        conn,
        "SELECT slug FROM rh_topics WHERE slug IN (SELECT value FROM json_each(?1))
         UNION SELECT slug FROM ros_wiki_topics WHERE slug IN (SELECT value FROM json_each(?1))",
        [candidates],
        |row| row.get(0),
    )?;
    let slug = choose_slug(&base, &kind.slug, |candidate| {
        if occupied.iter().any(|slug| slug == candidate) {
            SlugOccupant::Other
        } else {
            SlugOccupant::Free
        }
    })
    .ok_or_else(|| EntityStoreError::SlugTaken(prepared.name.clone()))?;
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
         VALUES (?1, 'entity', ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id.to_string(), slug, prepared.name, kind.id.to_string(),
            stamp(started), stamp(now), owner],
    ).map_err(query)?;
    Ok(id)
}

fn write_aliases(
    conn: &Connection,
    id: Uuid,
    prepared: &PreparedEntity,
    entities: &[EntityRow],
) -> Result<(), EntityStoreError> {
    let own = canonical_key(&prepared.name);
    for name in prepared
        .aliases
        .iter()
        .chain(std::iter::once(&prepared.name))
    {
        let key = canonical_key(name);
        if !key.is_empty()
            && key != own
            && entities
                .iter()
                .any(|row| row.id != id && folded(&raw_name(&row.name)) == key)
        {
            continue;
        }
        add_alias(conn, id, name)?;
    }
    Ok(())
}

pub(super) fn add_alias(conn: &Connection, id: Uuid, name: &str) -> Result<(), EntityStoreError> {
    let Some(key) = attribute_key(name) else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
         VALUES (?1, ?2, ?3, 'entity') ON CONFLICT (kind, alias_key) DO NOTHING",
        params![id.to_string(), name.trim(), key],
    )
    .map_err(query)?;
    Ok(())
}
