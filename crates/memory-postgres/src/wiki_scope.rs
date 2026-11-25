use std::path::PathBuf;

use kernel::tenancy::Scope;
use kernel::wiki::builder::WikiStoreError;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::PostgresMemory;

const PRIVATE: &str = "private";
const KEY_ATTEMPTS: u32 = 9;
const ROOT_OWNER: &str = "SELECT owner_user_id FROM rh_wiki_scopes WHERE directory = ''";

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct WikiScope {
    pub(crate) scope: String,
    pub(crate) owner_user_id: String,
    pub(crate) visibility: String,
    pub(crate) key_prefix: String,
    pub(crate) directory: String,
}

impl WikiScope {
    pub(crate) fn primary(&self) -> bool {
        self.directory.is_empty()
    }

    pub(crate) fn row_owner(&self) -> &str {
        if self.visibility == PRIVATE {
            &self.owner_user_id
        } else {
            &self.scope
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct PageHome {
    pub(crate) scope: Option<String>,
    pub(crate) directory: String,
    pub(crate) local: String,
}

fn failed(error: sqlx::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(error))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LegacyOwner {
    Owner(String),
    Unknown,
    Contested,
}

impl LegacyOwner {
    pub(crate) fn owner(&self) -> Option<&str> {
        match self {
            Self::Owner(owner) => Some(owner),
            Self::Unknown | Self::Contested => None,
        }
    }

    pub(crate) fn open(&self) -> bool {
        matches!(self, Self::Unknown) && !matches!(Scope::current(), Some(Scope::Viewer(_)))
    }
}

const HOMES: &str = "WITH homes AS (
         SELECT t.slug, t.scope, coalesce(s.directory, '') AS directory,
                coalesce(t.local_slug, t.slug) AS local,
                CASE WHEN t.scope IS NOT NULL
                     THEN coalesce(t.owner_user_id
                          = CASE WHEN s.visibility = 'private' THEN s.owner_user_id ELSE s.scope END, false)
                     WHEN $1::text IS NULL THEN $2
                     ELSE coalesce(t.owner_user_id = $1, false)
                END AS homed
           FROM rh_wiki_topics t LEFT JOIN rh_wiki_scopes s ON s.scope = t.scope)";

pub(crate) fn with_homes(query: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!("{HOMES} {query}"))
}

pub(crate) async fn page_home(
    conn: &mut PgConnection,
    key: &str,
    legacy: &LegacyOwner,
) -> Result<Option<PageHome>, WikiStoreError> {
    sqlx::query_as::<_, PageHome>(with_homes(
        "SELECT scope, directory, local FROM homes WHERE slug = $3 AND homed",
    ))
    .bind(legacy.owner())
    .bind(legacy.open())
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)
}

pub(crate) async fn primary_scope(
    conn: &mut PgConnection,
    legacy: &LegacyOwner,
) -> Result<Option<WikiScope>, WikiStoreError> {
    match legacy.owner() {
        Some(owner) => scope_for(conn, owner, PRIVATE, Some(owner))
            .await
            .map(|scope| Some(scope).filter(WikiScope::primary)),
        None => Ok(None),
    }
}

pub(crate) async fn page_place(
    conn: &mut PgConnection,
    key: &str,
) -> Result<Option<(Option<String>, String)>, WikiStoreError> {
    sqlx::query_as::<_, (Option<String>, String)>(
        "SELECT scope, coalesce(local_slug, slug) FROM rh_wiki_topics WHERE slug = $1",
    )
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)
}

async fn named(
    conn: &mut PgConnection,
    owner: &str,
    visibility: &str,
) -> Result<Option<WikiScope>, WikiStoreError> {
    sqlx::query_as::<_, WikiScope>(
        "SELECT scope, owner_user_id, visibility, key_prefix, directory FROM rh_wiki_scopes
          WHERE owner_user_id = $1 AND visibility = $2",
    )
    .bind(owner)
    .bind(visibility)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)
}

pub(crate) async fn summary_scope(
    conn: &mut PgConnection,
    summary: Uuid,
    primary: Option<&str>,
) -> Result<WikiScope, WikiStoreError> {
    let (owner, visibility): (String, String) = sqlx::query_as(
        "SELECT s.owner_user_id, coalesce(c.visibility, 'private')
           FROM rh_summaries s LEFT JOIN rh_conversations c ON c.id = s.conversation_id WHERE s.id = $1",
    )
    .bind(summary)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)?
    .ok_or(WikiStoreError::Missing(summary))?;
    scope_for(conn, &owner, &visibility, primary).await
}

pub(crate) async fn topic_scope(
    conn: &mut PgConnection,
    topic: Uuid,
    primary: Option<&str>,
) -> Result<Option<WikiScope>, WikiStoreError> {
    let found: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT owner_user_id,
                CASE WHEN kind = 'entity' THEN 'private' ELSE rh_topic_visibility(id) END
           FROM rh_topics WHERE id = $1",
    )
    .bind(topic)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)?;
    match found {
        Some((Some(owner), visibility)) => scope_for(conn, &owner, &visibility, primary)
            .await
            .map(Some),
        Some((None, _)) | None => match primary {
            Some(primary) => scope_for(conn, primary, PRIVATE, Some(primary))
                .await
                .map(Some),
            None => Ok(None),
        },
    }
}

#[must_use]
pub(crate) fn choose<'a>(origin: &'a WikiScope, page: &'a WikiScope) -> &'a WikiScope {
    if origin.scope == page.scope || (page.visibility == PRIVATE && origin.visibility != PRIVATE) {
        page
    } else {
        origin
    }
}

pub(crate) async fn overlay(
    conn: &mut PgConnection,
    scope: &WikiScope,
    local: &str,
) -> Result<Option<String>, WikiStoreError> {
    sqlx::query_scalar::<_, String>(
        "SELECT slug FROM rh_wiki_topics WHERE scope = $1 AND local_slug = $2",
    )
    .bind(&scope.scope)
    .bind(local)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)
}

async fn scope_for(
    conn: &mut PgConnection,
    owner: &str,
    visibility: &str,
    primary: Option<&str>,
) -> Result<WikiScope, WikiStoreError> {
    let holder = if visibility == PRIVATE {
        owner.to_owned()
    } else {
        visibility.to_owned()
    };
    if let Some(found) = named(conn, &holder, visibility).await? {
        return Ok(found);
    }
    let root = visibility == PRIVATE && primary.is_none_or(|primary| primary == holder);
    let own = if visibility == PRIVATE {
        (format!("{holder}-"), format!("users/{holder}"))
    } else {
        (format!("{holder}-"), format!("spaces/{holder}"))
    };
    let homes = root
        .then(|| (String::new(), String::new()))
        .into_iter()
        .chain([own]);
    for (prefix, directory) in homes {
        sqlx::query(
            "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
             VALUES ($1, $1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(&holder)
        .bind(visibility)
        .bind(&prefix)
        .bind(&directory)
        .execute(&mut *conn)
        .await
        .map_err(failed)?;
        if let Some(found) = named(conn, &holder, visibility).await? {
            return Ok(found);
        }
    }
    Err(WikiStoreError::InvalidPatch(format!(
        "no wiki scope for {holder}"
    )))
}

pub(crate) async fn every_owner<'c>(
    tx: &'c mut Transaction<'_, Postgres>,
) -> Result<Transaction<'c, Postgres>, WikiStoreError> {
    let mut savepoint = sqlx::Connection::begin(&mut **tx).await.map_err(failed)?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', true)")
        .execute(&mut *savepoint)
        .await
        .map_err(failed)?;
    Ok(savepoint)
}

pub(crate) async fn scope_named(
    conn: &mut PgConnection,
    scope: &str,
) -> Result<Option<WikiScope>, WikiStoreError> {
    sqlx::query_as::<_, WikiScope>(
        "SELECT scope, owner_user_id, visibility, key_prefix, directory FROM rh_wiki_scopes WHERE scope = $1",
    )
    .bind(scope)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)
}

pub(crate) async fn page_key(
    conn: &mut PgConnection,
    scope: &WikiScope,
    local: &str,
) -> Result<String, WikiStoreError> {
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT slug FROM rh_wiki_topics
          WHERE coalesce(local_slug, slug) = $1 AND (scope = $2 OR ($3 AND scope IS NULL))",
    )
    .bind(local)
    .bind(&scope.scope)
    .bind(scope.primary())
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)?;
    if let Some(key) = existing {
        return Ok(key);
    }
    let base = format!("{}{local}", scope.key_prefix);
    for attempt in 1..=KEY_ATTEMPTS {
        let candidate = if attempt == 1 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = $1)")
                .bind(&candidate)
                .fetch_one(&mut *conn)
                .await
                .map_err(failed)?;
        if !taken {
            return Ok(candidate);
        }
    }
    Err(WikiStoreError::InvalidPatch(format!(
        "no free page key for {base}"
    )))
}

impl PostgresMemory {
    pub(crate) fn wiki_home(&self, directory: &str) -> Result<PathBuf, WikiStoreError> {
        let root = self
            .wiki_directory
            .as_ref()
            .ok_or(WikiStoreError::DirectoryMissing)?;
        Ok(if directory.is_empty() {
            root.clone()
        } else {
            root.join(directory)
        })
    }

    pub(crate) fn wiki_primary(&self) -> Option<&str> {
        self.wiki_owner.as_ref().map(kernel::owner::Owner::as_str)
    }

    pub(crate) async fn legacy_owner(&self) -> Result<LegacyOwner, WikiStoreError> {
        let recorded = Scope::Maintenance
            .run(sqlx::query_scalar::<_, String>(ROOT_OWNER).fetch_optional(&self.pool))
            .await
            .map_err(failed)?;
        Ok(self.legacy(recorded))
    }

    pub(crate) async fn held_legacy_owner(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<LegacyOwner, WikiStoreError> {
        let mut every_owner = every_owner(tx).await?;
        let recorded = sqlx::query_scalar::<_, String>(ROOT_OWNER)
            .fetch_optional(&mut *every_owner)
            .await
            .map_err(failed)?;
        every_owner.rollback().await.map_err(failed)?;
        Ok(self.legacy(recorded))
    }

    fn legacy(&self, recorded: Option<String>) -> LegacyOwner {
        match (recorded, self.wiki_primary()) {
            (Some(recorded), Some(configured)) if recorded != configured => LegacyOwner::Contested,
            (Some(recorded), _) => LegacyOwner::Owner(recorded),
            (None, Some(configured)) => LegacyOwner::Owner(configured.to_owned()),
            (None, None) => LegacyOwner::Unknown,
        }
    }
}
