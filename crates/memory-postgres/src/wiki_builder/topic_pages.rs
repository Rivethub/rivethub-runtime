use async_trait::async_trait;
use chrono::{DateTime, Utc};
use kernel::topics::{PageChange, PageEdit, PageSection, TopicId, TopicPage, TopicPageStore};
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::{WikiConsolidateStore, WikiFold};
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::{ArticleSection, FoldMark, WikiPage, WikiSectionRef};

use super::overlays::Placed;
use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_scope::{PageHome, WikiScope, page_home, scope_named};
use crate::wiki_writer::{LockedRepository, WikiRepository};

#[derive(Debug, sqlx::FromRow)]
struct PageRow {
    title: String,
    current_state: String,
    article: String,
}

#[async_trait]
impl TopicPageStore for PostgresMemory {
    async fn topic_page(&self, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError> {
        let placed = self.place_note_page(slug, None).await?;
        if !placed.canonical {
            return self.overlay_page(slug, &placed).await;
        }
        let (home, _) = self.home(slug).await?;
        let repository = WikiRepository::for_store(self.wiki_home(&home.directory)?)
            .lock()
            .await?;
        let read = repository
            .read(&home.local)
            .await
            .map_err(WikiStoreError::from);
        match settle(read, repository.release().await)? {
            Some(page) => topic_page(page).map(|page| {
                Some(TopicPage {
                    slug: slug.clone(),
                    ..page
                })
            }),
            None => self.row_page(slug).await,
        }
    }

    async fn page_taken(&self, slug: &WikiSlug) -> Result<bool, WikiStoreError> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = $1)
                 OR EXISTS (SELECT 1 FROM rh_wiki_redirects WHERE from_slug = $1)",
        )
        .bind(slug.as_ref())
        .fetch_one(&self.pool)
        .await
        .map_err(query_error)
    }

    async fn save_topic_page(
        &self,
        topic: TopicId,
        page: &TopicPage,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let placed = self.place_note_page(&page.slug, Some(topic.uuid())).await?;
        let directory = placed
            .scope
            .as_ref()
            .map_or(String::new(), |scope| scope.directory.clone());
        let mut repository = WikiRepository::for_store(self.wiki_home(&directory)?)
            .lock()
            .await?;
        let saved = self.save_locked(&mut repository, page, &placed, now).await;
        settle(saved, repository.release().await)
    }

    async fn edit_page(
        &self,
        slug: &WikiSlug,
        edit: &PageEdit,
        now: DateTime<Utc>,
    ) -> Result<bool, WikiStoreError> {
        let bound = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = $1 AND topic_id IS NOT NULL)",
        )
        .bind(slug.as_ref())
        .fetch_one(&self.pool)
        .await
        .map_err(query_error)?;
        if !bound {
            return Ok(false);
        }
        let (home, scope) = self.home(slug).await?;
        let mut repository = WikiRepository::for_store(self.wiki_home(&home.directory)?)
            .lock()
            .await?;
        let placed = Placed {
            scope,
            key: slug.as_ref().to_owned(),
            local: home.local,
            canonical: true,
            bind: None,
        };
        let edited = self
            .edit_locked(&mut repository, slug, &placed, edit, now)
            .await;
        settle(edited, repository.release().await).map(|()| true)
    }

    async fn change_page(
        &self,
        root: TopicId,
        slug: &WikiSlug,
        change: &PageChange<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let placed = self.place_note_page(slug, Some(root.uuid())).await?;
        let directory = placed
            .scope
            .as_ref()
            .map_or(String::new(), |scope| scope.directory.clone());
        let mut repository = WikiRepository::for_store(self.wiki_home(&directory)?)
            .lock()
            .await?;
        let changed = self
            .change_locked(&mut repository, slug, &placed, change, now)
            .await;
        settle(changed, repository.release().await)
    }

    async fn redirect_page(
        &self,
        from: &WikiSlug,
        to: &WikiSlug,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        sqlx::query(
            "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at)
             SELECT $1, $2, $3
              WHERE NOT EXISTS (SELECT 1 FROM rh_wiki_topics WHERE slug = $1)
             ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug
              WHERE rh_wiki_redirects.to_slug <> EXCLUDED.to_slug",
        )
        .bind(from.as_ref())
        .bind(to.as_ref())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(query_error)?;
        Ok(())
    }

    async fn fold_page(
        &self,
        child: &WikiSlug,
        into: &WikiSlug,
        mark: FoldMark,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let fold = WikiFold {
            cluster: into.as_ref().to_owned(),
            parent: into.as_ref().to_owned(),
            children: vec![child.as_ref().to_owned()],
            mark,
        };
        self.fold_wiki(&fold, now).await.map(drop)
    }

    async fn remove_page(&self, slug: &WikiSlug) -> Result<(), WikiStoreError> {
        let message = format!("wiki({}): removed after a tidy review", slug.as_ref());
        let removed = {
            let mut repository = self.repository().await?;
            let removed = repository
                .remove(&[slug.as_ref().to_owned()], &message)
                .await
                .map_err(WikiStoreError::from);
            settle(removed, repository.release().await)
        };
        removed?;
        sqlx::query("DELETE FROM rh_wiki_topics WHERE slug = $1")
            .bind(slug.as_ref())
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(())
    }
}

impl PostgresMemory {
    async fn repository(&self) -> Result<LockedRepository, WikiStoreError> {
        let directory = self
            .wiki_directory
            .as_ref()
            .ok_or(WikiStoreError::DirectoryMissing)?;
        Ok(WikiRepository::for_store(directory.clone()).lock().await?)
    }

    async fn row_page(&self, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError> {
        let row = sqlx::query_as::<_, PageRow>(
            "SELECT title, current_state, article FROM rh_wiki_topics WHERE slug = $1",
        )
        .bind(slug.as_ref())
        .fetch_optional(&self.pool)
        .await
        .map_err(query_error)?;
        Ok(row.map(|row| TopicPage {
            slug: slug.clone(),
            title: row.title,
            summary: row.current_state,
            article: row.article,
            sections: Vec::new(),
        }))
    }

    async fn change_locked(
        &self,
        repository: &mut LockedRepository,
        slug: &WikiSlug,
        placed: &Placed,
        change: &PageChange<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let before = match repository.read(&placed.local).await? {
            Some(stored) => TopicPage {
                slug: slug.clone(),
                ..topic_page(stored)?
            },
            None if !placed.canonical => self
                .overlay_page(slug, placed)
                .await?
                .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()))?,
            None => return Err(WikiStoreError::TopicMissing(slug.as_ref().to_owned())),
        };
        let mut after = before.clone();
        change(&mut after);
        if after == before {
            return Ok(());
        }
        self.save_locked(repository, &after, placed, now).await
    }

    async fn home(&self, slug: &WikiSlug) -> Result<(PageHome, Option<WikiScope>), WikiStoreError> {
        let mut connection = self.pool.acquire().await.map_err(query_error)?;
        let home = page_home(&mut connection, slug.as_ref()).await?;
        let scope = match &home.scope {
            Some(name) => scope_named(&mut connection, name).await?,
            None => None,
        };
        Ok((home, scope))
    }

    async fn edit_locked(
        &self,
        repository: &mut LockedRepository,
        slug: &WikiSlug,
        placed: &Placed,
        edit: &PageEdit,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        if let Some(id) = kernel::wiki::origin::NoteOrigin::current() {
            crate::summary_replace::shared(&mut tx, &[id])
                .await
                .map_err(super::fence_error)?;
        }
        lock_slug(&mut tx, slug.as_ref()).await?;
        let mut stored = repository
            .read(&placed.local)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()))?;
        edit.apply_to(&mut stored, now);
        let git_sha = repository
            .fold(&mut stored, &[], &PageEdit::commit_message(slug), now)
            .await?;
        topics::store(
            &mut tx,
            &stored,
            slug.as_ref(),
            placed.scope.as_ref(),
            &git_sha,
            now,
        )
        .await?;
        tx.commit().await.map_err(query_error)
    }

    async fn save_locked(
        &self,
        repository: &mut LockedRepository,
        page: &TopicPage,
        placed: &Placed,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        if let Some(id) = kernel::wiki::origin::NoteOrigin::current() {
            crate::summary_replace::shared(&mut tx, &[id])
                .await
                .map_err(super::fence_error)?;
        }
        lock_slug(&mut tx, &placed.key).await?;
        let local = WikiSlug::try_from(placed.local.clone())
            .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
        let mut stored = match repository.read(&placed.local).await? {
            Some(stored) => stored,
            None => WikiPage::empty(local, page.title.clone()),
        };
        page.title.clone_into(&mut stored.meta.title);
        page.summary.clone_into(&mut stored.current_state);
        page.article.clone_into(&mut stored.article);
        stored.meta.sections = page
            .sections
            .iter()
            .filter_map(|section| {
                section.topic.map(|topic| WikiSectionRef {
                    topic: topic.to_string(),
                    heading: section.heading.clone(),
                })
            })
            .collect();
        stored.extra_sections = page
            .sections
            .iter()
            .map(|section| ArticleSection {
                heading: section.heading.clone(),
                body: section.body.clone(),
            })
            .collect();
        let message = format!("wiki({}): sync topic page\n\nPipeline: wiki-v7", placed.key);
        let git_sha = repository.fold(&mut stored, &[], &message, now).await?;
        topics::store(
            &mut tx,
            &stored,
            &placed.key,
            placed.scope.as_ref(),
            &git_sha,
            now,
        )
        .await?;
        if let Some(topic) = placed.bind {
            sqlx::query(
                "UPDATE rh_wiki_topics SET topic_id = NULL WHERE topic_id = $1 AND slug <> $2",
            )
            .bind(topic)
            .bind(&placed.key)
            .execute(&mut *tx)
            .await
            .map_err(query_error)?;
            sqlx::query("UPDATE rh_wiki_topics SET topic_id = $1 WHERE slug = $2")
                .bind(topic)
                .bind(&placed.key)
                .execute(&mut *tx)
                .await
                .map_err(query_error)?;
        }
        tx.commit().await.map_err(query_error)
    }
}

pub(super) fn topic_page(page: WikiPage) -> Result<TopicPage, WikiStoreError> {
    let refs = page.meta.sections;
    let sections = page
        .extra_sections
        .into_iter()
        .map(|section| {
            let topic = refs
                .iter()
                .find(|known| known.heading == section.heading)
                .map(|known| known.topic.parse::<TopicId>())
                .transpose()
                .map_err(|source| WikiStoreError::Query(Box::new(source)))?;
            Ok(PageSection {
                topic,
                heading: section.heading,
                body: section.body,
            })
        })
        .collect::<Result<_, WikiStoreError>>()?;
    Ok(TopicPage {
        slug: page.meta.slug,
        title: page.meta.title,
        summary: page.current_state,
        article: page.article,
        sections,
    })
}
