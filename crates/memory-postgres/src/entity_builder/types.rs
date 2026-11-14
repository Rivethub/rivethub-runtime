use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_match::TYPE_OBSERVATION_WINDOW;
use kernel::wiki::entity_match::dominant_type;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::TypeBrief;
use kernel::wiki::entity_pass::attribute_key;
use kernel::wiki::entity_pass::compact_key;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;

use super::sql::RECENT_TYPES;
use super::sql::REMEMBER_TYPE;
use super::sql::TYPE_ALIASES;
use super::sql::TYPE_EXAMPLES;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct TypeRow {
    pub(super) id: Uuid,
    pub(super) parent_id: Option<Uuid>,
    pub(super) name: String,
    pub(super) slug: String,
}

pub(super) async fn type_rows(conn: &mut PgConnection) -> Result<Vec<TypeRow>, EntityStoreError> {
    sqlx::query_as::<_, TypeRow>(
        "SELECT id, parent_id, name, slug FROM rh_topics WHERE kind = 'type'",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(query_error)
}

pub(super) async fn type_paths(pool: &sqlx::PgPool) -> Result<Vec<String>, EntityStoreError> {
    let mut conn = pool.acquire().await.map_err(query_error)?;
    let rows = type_rows(&mut conn).await?;
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

pub(super) async fn ensure_type(
    tx: &mut Transaction<'_, Postgres>,
    wanted: &str,
    rows: &mut Vec<TypeRow>,
    owner: &str,
    now: DateTime<Utc>,
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
    let inserted = sqlx::query(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at, owner_user_id)
         VALUES ($1, 'type', $2, $3, $4, $4, $5)",
    )
    .bind(id)
    .bind(&slug)
    .bind(name)
    .bind(now)
    .bind(owner)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {
            let row = TypeRow {
                id,
                parent_id: None,
                name: name.to_owned(),
                slug,
            };
            rows.push(row.clone());
            Ok(Some(row))
        }
        Err(error) => {
            let wrapped = query_error(error);
            if !unique_violation(&wrapped) {
                return Err(wrapped);
            }
            *rows = type_rows(tx).await?;
            Ok(rows.iter().find(|row| row.slug == slug).cloned())
        }
    }
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

fn unique_violation(error: &EntityStoreError) -> bool {
    let EntityStoreError::Query(source) = error else {
        return false;
    };
    source.downcast_ref::<sqlx::Error>().is_some_and(|error| {
        error
            .as_database_error()
            .is_some_and(|db| db.code().as_deref() == Some("23505"))
    })
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

pub(super) async fn observed_types(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
) -> Result<Vec<String>, EntityStoreError> {
    sqlx::query_scalar(RECENT_TYPES)
        .bind(entity)
        .bind(observation_window()?)
        .fetch_all(&mut **tx)
        .await
        .map_err(query_error)
}

fn observation_window() -> Result<i64, EntityStoreError> {
    i64::try_from(TYPE_OBSERVATION_WINDOW).map_err(|error| {
        EntityStoreError::Query(Box::new(std::io::Error::other(error.to_string())))
    })
}

pub(super) async fn remember_type(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    summary: Uuid,
    type_name: &str,
    observed_at: DateTime<Utc>,
    owner: &str,
) -> Result<(), EntityStoreError> {
    let trimmed = type_name.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    sqlx::query(REMEMBER_TYPE)
        .bind(entity)
        .bind(summary)
        .bind(trimmed)
        .bind(observed_at)
        .bind(owner)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct AliasRow {
    topic_id: Uuid,
    alias: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ExampleRow {
    parent_id: Uuid,
    name: String,
}

pub(super) async fn type_briefs(pool: &sqlx::PgPool) -> Result<Vec<TypeBrief>, EntityStoreError> {
    let mut conn = pool.acquire().await.map_err(query_error)?;
    let rows = type_rows(&mut conn).await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let aliases = sqlx::query_as::<_, AliasRow>(TYPE_ALIASES)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(query_error)?;
    let examples = sqlx::query_as::<_, ExampleRow>(TYPE_EXAMPLES)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(query_error)?;
    let mut briefs = Vec::new();
    for row in &rows {
        briefs.push(TypeBrief {
            name: type_path(row, &rows),
            slug: row.slug.clone(),
            aliases: aliases
                .iter()
                .filter(|alias| alias.topic_id == row.id)
                .map(|alias| alias.alias.clone())
                .collect(),
            examples: examples
                .iter()
                .filter(|example| example.parent_id == row.id)
                .map(|example| example.name.clone())
                .collect(),
        });
    }
    briefs.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(briefs)
}

pub(super) async fn remember_type_alias(
    pool: &sqlx::PgPool,
    type_name: &str,
    alias: &str,
) -> Result<(), EntityStoreError> {
    let alias = alias.trim();
    let Some(key) = attribute_key(alias) else {
        return Ok(());
    };
    let mut conn = pool.acquire().await.map_err(query_error)?;
    let rows = type_rows(&mut conn).await?;
    let Some(row) = rows.iter().find(|row| {
        type_path(row, &rows) == type_name || row.name == type_name || row.slug == type_name
    }) else {
        return Ok(());
    };
    if compact_key(alias) == compact_key(&row.name) || compact_key(alias) == compact_key(&row.slug)
    {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
         VALUES ($1, $2, $3, 'type')
         ON CONFLICT (kind, alias_key) DO NOTHING",
    )
    .bind(row.id)
    .bind(alias)
    .bind(key)
    .execute(&mut *conn)
    .await
    .map_err(query_error)?;
    Ok(())
}

pub(super) async fn retype_entity(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    types: &[TypeRow],
) -> Result<(), EntityStoreError> {
    let recent = sqlx::query_scalar::<_, String>(RECENT_TYPES)
        .bind(entity)
        .bind(observation_window()?)
        .fetch_all(&mut **tx)
        .await
        .map_err(query_error)?;
    let Some(winner) = dominant_type(&recent) else {
        return Ok(());
    };
    let Some(kind) = match_type(winner, types) else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE rh_topics
            SET parent_id = $2, updated_at = now()
          WHERE id = $1 AND kind = 'entity' AND parent_id IS DISTINCT FROM $2",
    )
    .bind(entity)
    .bind(kind.id)
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}
