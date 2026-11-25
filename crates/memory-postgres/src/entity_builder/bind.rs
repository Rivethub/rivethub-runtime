use kernel::wiki::entity_pass::EntityStoreError;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;
use super::error::store_error;
use crate::wiki_scope::PRIVATE;
use crate::wiki_scope::WikiScope;

const LOCAL_ATTEMPTS: u32 = 9;

pub(super) struct PageKey {
    pub(super) key: String,
    pub(super) local: String,
}

#[derive(Debug, sqlx::FromRow)]
struct BoundPage {
    slug: String,
    local: String,
    owner_user_id: Option<String>,
    scope: Option<String>,
    visibility: String,
}

impl BoundPage {
    fn at_home(&self, home: &WikiScope) -> bool {
        self.scope.as_deref() == Some(home.scope.as_str())
            && self.owner_user_id.as_deref() == Some(home.row_owner())
    }

    fn movable(&self) -> bool {
        self.scope.is_none() || self.visibility != PRIVATE
    }
}

pub(super) async fn page_key(
    tx: &mut Transaction<'_, Postgres>,
    topic: Uuid,
    slug: &str,
    home: &WikiScope,
) -> Result<PageKey, EntityStoreError> {
    let bound = sqlx::query_as::<_, BoundPage>(
        "SELECT slug, coalesce(local_slug, slug) AS local, owner_user_id, scope, visibility
           FROM rh_wiki_topics WHERE topic_id = $1",
    )
    .bind(topic)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)?;
    if let Some(page) = bound {
        if !page.at_home(home) && !page.movable() {
            return Err(EntityStoreError::SlugTaken(format!(
                "entity page {} is outside the {} wiki",
                page.slug, home.scope
            )));
        }
        lock(tx, &page.slug).await?;
        let local = if page.at_home(home) {
            page.local
        } else {
            move_home(tx, &page, home).await?
        };
        return Ok(PageKey {
            key: page.slug,
            local,
        });
    }
    for attempt in 1..=LOCAL_ATTEMPTS {
        let local = if attempt == 1 {
            slug.to_owned()
        } else {
            format!("{slug}-{attempt}")
        };
        let key = crate::wiki_scope::page_key(tx, home, &local)
            .await
            .map_err(store_error)?;
        lock(tx, &key).await?;
        if open_to_entity(tx, &key, home).await? {
            return Ok(PageKey { key, local });
        }
    }
    Err(EntityStoreError::SlugTaken(format!(
        "no free entity page key for {slug} in the {} wiki",
        home.scope
    )))
}

async fn move_home(
    tx: &mut Transaction<'_, Postgres>,
    page: &BoundPage,
    home: &WikiScope,
) -> Result<String, EntityStoreError> {
    for attempt in 1..=LOCAL_ATTEMPTS {
        let local = if attempt == 1 {
            page.local.clone()
        } else {
            format!("{}-{attempt}", page.local)
        };
        let moved = sqlx::query(
            "UPDATE rh_wiki_topics SET scope = $2, local_slug = $3, owner_user_id = $4, visibility = $5
              WHERE slug = $1
                AND NOT EXISTS (SELECT 1 FROM rh_wiki_topics
                                 WHERE scope = $2 AND local_slug = $3 AND slug <> $1)",
        )
        .bind(&page.slug)
        .bind(&home.scope)
        .bind(&local)
        .bind(home.row_owner())
        .bind(&home.visibility)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
        if moved.rows_affected() == 1 {
            return Ok(local);
        }
    }
    Err(EntityStoreError::SlugTaken(format!(
        "no free page name for {} in the {} wiki",
        page.slug, home.scope
    )))
}

async fn lock(tx: &mut Transaction<'_, Postgres>, key: &str) -> Result<(), EntityStoreError> {
    crate::wiki_builder::lock_slug(tx, key)
        .await
        .map_err(store_error)
}

async fn open_to_entity(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    home: &WikiScope,
) -> Result<bool, EntityStoreError> {
    sqlx::query_scalar::<_, bool>(
        "SELECT NOT EXISTS (
                    SELECT 1 FROM rh_wiki_topics
                     WHERE slug = $1
                       AND (topic_id IS NOT NULL
                            OR owner_user_id IS DISTINCT FROM $2
                            OR NOT coalesce(scope = $3, $4)))
            AND NOT EXISTS (SELECT 1 FROM rh_topics WHERE slug = $1 AND kind <> 'entity')",
    )
    .bind(key)
    .bind(home.row_owner())
    .bind(&home.scope)
    .bind(home.primary())
    .fetch_one(&mut **tx)
    .await
    .map_err(query_error)
}

pub(super) async fn bind_page(
    tx: &mut Transaction<'_, Postgres>,
    topic: Uuid,
    slug: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE rh_wiki_topics SET topic_id = NULL WHERE topic_id = $1 AND slug <> $2")
        .bind(topic)
        .bind(slug)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE rh_wiki_topics SET topic_id = $1 WHERE slug = $2")
        .bind(topic)
        .bind(slug)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
