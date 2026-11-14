use std::path::PathBuf;

use kernel::wiki::builder::WikiStoreError;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::PostgresMemory;

const PRIVATE: &str = "private";
const KEY_ATTEMPTS: u32 = 9;

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

pub(crate) async fn page_home(
    conn: &mut PgConnection,
    key: &str,
) -> Result<PageHome, WikiStoreError> {
    let found = sqlx::query_as::<_, PageHome>(
        "SELECT t.scope, coalesce(s.directory, '') AS directory, coalesce(t.local_slug, t.slug) AS local
           FROM rh_wiki_topics t LEFT JOIN rh_wiki_scopes s ON s.scope = t.scope WHERE t.slug = $1",
    )
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(failed)?;
    Ok(found.unwrap_or_else(|| PageHome {
        scope: None,
        directory: String::new(),
        local: key.to_owned(),
    }))
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
        "SELECT owner_user_id, rh_topic_visibility(id) FROM rh_topics WHERE id = $1",
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
}
