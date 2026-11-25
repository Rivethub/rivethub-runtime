use kernel::tenancy::Scope;
use kernel::topics::TopicPage;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::origin::NoteOrigin;
use kernel::wiki::page::WikiSlug;
use sqlx::PgConnection;
use uuid::Uuid;

use super::{query_error, settle};
use crate::PostgresMemory;
use crate::wiki_scope::{
    LegacyOwner, WikiScope, choose, overlay, page_home, scope_named, summary_scope, topic_scope,
};
use crate::wiki_writer::WikiRepository;

pub(super) struct Placed {
    pub(super) scope: Option<WikiScope>,
    pub(super) key: String,
    pub(super) local: String,
    pub(super) canonical: bool,
    pub(super) fresh: bool,
    pub(super) bind: Option<Uuid>,
}

async fn root(conn: &mut PgConnection) -> Result<Option<WikiScope>, WikiStoreError> {
    sqlx::query_as::<_, WikiScope>(
        "SELECT scope, owner_user_id, visibility, key_prefix, directory FROM rh_wiki_scopes WHERE directory = ''",
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(query_error)
}

impl PostgresMemory {
    async fn page_scope(
        &self,
        conn: &mut PgConnection,
        slug: &str,
        topic: Option<Uuid>,
        legacy: &LegacyOwner,
    ) -> Result<(Option<WikiScope>, String, bool), WikiStoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = $1)")
                .bind(slug)
                .fetch_one(&mut *conn)
                .await
                .map_err(query_error)?;
        if !exists && let Some(topic) = topic {
            return Ok((
                topic_scope(conn, topic, self.wiki_primary()).await?,
                slug.to_owned(),
                false,
            ));
        }
        let home = match page_home(conn, slug, legacy).await? {
            Some(home) => home,
            None if exists => return Err(WikiStoreError::TopicMissing(slug.to_owned())),
            None => return Ok((root(conn).await?, slug.to_owned(), false)),
        };
        let scope = match &home.scope {
            Some(name) => scope_named(conn, name).await?,
            None => root(conn).await?,
        };
        Ok((scope, home.local, exists))
    }

    pub(super) async fn place_note_page(
        &self,
        slug: &WikiSlug,
        topic: Option<Uuid>,
    ) -> Result<Placed, WikiStoreError> {
        let legacy = self.legacy_owner().await?;
        let mut conn = self.pool.acquire().await.map_err(query_error)?;
        let (page, local, exists) = self
            .page_scope(&mut conn, slug.as_ref(), topic, &legacy)
            .await?;
        let canonical = Placed {
            scope: page.clone(),
            key: slug.as_ref().to_owned(),
            local: local.clone(),
            canonical: true,
            fresh: !exists && matches!(Scope::current(), Some(Scope::Viewer(_))),
            bind: topic,
        };
        let (true, Some(summary), Some(page)) = (exists, NoteOrigin::current(), page) else {
            return Ok(canonical);
        };
        let origin = summary_scope(&mut conn, summary, self.wiki_primary()).await?;
        let target = choose(&origin, &page);
        if target.scope == page.scope {
            return Ok(canonical);
        }
        let key = match overlay(&mut conn, target, &local).await? {
            Some(key) => key,
            None => crate::wiki_scope::page_key(&mut conn, target, &local).await?,
        };
        Ok(Placed {
            scope: Some(target.clone()),
            key,
            local,
            canonical: false,
            fresh: false,
            bind: None,
        })
    }

    pub(super) async fn overlay_page(
        &self,
        slug: &WikiSlug,
        placed: &Placed,
    ) -> Result<Option<TopicPage>, WikiStoreError> {
        let directory = placed
            .scope
            .as_ref()
            .map_or(String::new(), |scope| scope.directory.clone());
        let found = {
            let repository = WikiRepository::for_store(self.wiki_home(&directory)?)
                .lock()
                .await?;
            let read = repository
                .read(&placed.local)
                .await
                .map_err(WikiStoreError::from);
            settle(read, repository.release().await)?
        };
        if let Some(page) = found {
            return super::topic_pages::topic_page(page).map(|page| {
                Some(TopicPage {
                    slug: slug.clone(),
                    ..page
                })
            });
        }
        self.empty_overlay_page(slug).await
    }

    pub(super) async fn empty_overlay_page(
        &self,
        slug: &WikiSlug,
    ) -> Result<Option<TopicPage>, WikiStoreError> {
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM rh_wiki_topics WHERE slug = $1")
                .bind(slug.as_ref())
                .fetch_optional(&self.pool)
                .await
                .map_err(query_error)?;
        Ok(title.map(|title| TopicPage::empty(slug.clone(), &title)))
    }
}
