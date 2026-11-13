use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_match::TYPE_OBSERVATION_WINDOW;
use kernel::wiki::entity_match::dominant_type;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::compact_key;
use sqlx::PgConnection;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;

use super::sql::RECENT_TYPES;
use super::sql::REMEMBER_TYPE;

#[derive(Debug, sqlx::FromRow)]
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
