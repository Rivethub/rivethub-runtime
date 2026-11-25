use async_trait::async_trait;
use jiff::Timestamp;
use kernel::topics::{Topic, TopicPage};
use kernel::wiki::builder::{WikiStoreError, WikiWriteError};
use kernel::wiki::consolidate::{WikiConsolidateStore, WikiFold, WikiTopicStats};
use kernel::wiki::page::{FoldMark, FoldedTopic, WikiPage, WikiSlug, fold_page, fold_text};
use sqlx::{Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};

use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_scope::{PageHome, page_home, scope_named};
use crate::wiki_writer::{LockedRepository, WikiRepository};

#[derive(Debug, sqlx::FromRow)]
struct StatsRow {
    slug: String,
    title: String,
    citations: i64,
    history_count: i32,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    bound: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct TextRow {
    title: String,
    current_state: String,
    article: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    related: Vec<String>,
}

#[async_trait]
impl WikiConsolidateStore for PostgresMemory {
    async fn wiki_topic_stats(&self) -> Result<Vec<WikiTopicStats>, WikiStoreError> {
        let rows = sqlx::query_as::<_, StatsRow>(
            "SELECT t.slug, t.title, t.history_count, t.created_at, t.topic_id IS NOT NULL AS bound,
                    (SELECT count(*) FROM rh_wiki_citations c WHERE c.topic_slug = t.slug) AS citations
               FROM rh_wiki_topics t
              WHERE t.scope IS NULL OR t.scope = (SELECT s.scope FROM rh_wiki_scopes s WHERE s.directory = '')
              ORDER BY t.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(WikiTopicStats {
                    slug: row.slug,
                    title: row.title,
                    citations: count(row.citations)?,
                    history: count(row.history_count.into())?,
                    created_at: row.created_at,
                    bound: row.bound,
                })
            })
            .collect()
    }

    async fn fold_wiki(
        &self,
        fold: &WikiFold,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        self.fold_with(fold, None, now).await
    }
}

impl PostgresMemory {
    pub(super) async fn fold_with(
        &self,
        fold: &WikiFold,
        section: Option<&Topic>,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let homes = self.fold_homes(fold).await?;
        let mut repositories = Repositories::lock(self, &homes).await?;
        let folded = self
            .fold_pages(&mut repositories, fold, &homes, section, now)
            .await;
        settle(folded, repositories.release().await)
    }

    async fn fold_homes(&self, fold: &WikiFold) -> Result<Homes, WikiStoreError> {
        let mut connection = self.pool.acquire().await.map_err(query_error)?;
        let mut homes = Homes::new();
        for key in std::iter::once(&fold.parent).chain(&fold.children) {
            let home = page_home(&mut connection, key).await?;
            homes.insert(key.clone(), home);
        }
        Ok(homes)
    }

    async fn fold_pages(
        &self,
        repositories: &mut Repositories,
        fold: &WikiFold,
        homes: &Homes,
        section: Option<&Topic>,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let keys: Vec<&str> = homes.keys().map(String::as_str).collect();
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT summary_id FROM rh_wiki_citations WHERE topic_slug = ANY($1)
             UNION SELECT source_id FROM rh_wiki_provenance
              WHERE topic_slug = ANY($1) AND source_kind = 'summary'",
        )
        .bind(&keys)
        .fetch_all(&mut *tx)
        .await
        .map_err(query_error)?;
        let live = crate::summary_replace::shared_visible(&mut tx, &ids)
            .await
            .map_err(super::fence_error)?;
        let live: Vec<_> = live.into_iter().map(|input| input.id).collect();
        lock_homes(&mut tx, homes).await?;
        let parent = home_of(homes, &fold.parent)?;
        let row = text(&mut tx, &fold.parent)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(fold.parent.clone()))?;
        let mut page = match repositories
            .at(&parent.directory)?
            .read(&parent.local)
            .await?
        {
            Some(page) => page,
            None => indexed_page(&parent.local, row)?,
        };
        let mut removed = Removed::new();
        let mut folded = Vec::new();
        for child in fold.children.iter().filter(|child| **child != fold.parent) {
            let Some(mut row) = text(&mut tx, child).await? else {
                continue;
            };
            let home = home_of(homes, child)?;
            let mut file = repositories.at(&home.directory)?.read(&home.local).await?;
            if let Some(owner) = section {
                take_section(&mut page, &mut row, file.as_mut(), owner)?;
            }
            if file.is_some() {
                removed
                    .entry(home.directory.clone())
                    .or_default()
                    .push(home.local.clone());
            }
            absorb(&mut page, &home.local, row, file, fold.mark);
            folded.push(child.clone());
        }
        if folded.is_empty() {
            tx.commit().await.map_err(query_error)?;
            return Ok(folded);
        }
        let retired: Vec<&str> = folded.iter().map(String::as_str).collect();
        crate::wiki_redirect::hold_pages(&mut tx, &retired, Some(&fold.parent)).await?;
        for child in &folded {
            retire(&mut tx, child, &fold.parent, now, &live).await?;
        }
        let message = format!(
            "wiki({}): consolidate {}\n\nPipeline: wiki-v7",
            fold.parent,
            folded.join(", ")
        );
        let git_sha = repositories
            .write(&mut page, &parent.directory, removed, &message, now)
            .await?;
        let scope = match &parent.scope {
            Some(name) => scope_named(&mut tx, name).await?,
            None => None,
        };
        topics::store(&mut tx, &page, &fold.parent, scope.as_ref(), &git_sha, now).await?;
        tx.commit().await.map_err(query_error)?;
        Ok(folded)
    }
}

type Homes = BTreeMap<String, PageHome>;
type Removed = BTreeMap<String, Vec<String>>;

async fn lock_homes(
    tx: &mut Transaction<'_, Postgres>,
    homes: &Homes,
) -> Result<(), WikiStoreError> {
    for key in homes.keys() {
        lock_slug(tx, key).await?;
    }
    for (key, home) in homes {
        if page_home(tx, key).await? != *home {
            return Err(WikiStoreError::InvalidPatch(format!(
                "wiki page {key} moved during a fold"
            )));
        }
    }
    Ok(())
}

fn home_of<'a>(homes: &'a Homes, key: &str) -> Result<&'a PageHome, WikiStoreError> {
    homes
        .get(key)
        .ok_or_else(|| WikiStoreError::TopicMissing(key.to_owned()))
}

fn indexed_page(local: &str, row: TextRow) -> Result<WikiPage, WikiStoreError> {
    let slug = WikiSlug::try_from(local.to_owned())
        .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))?;
    let mut page = WikiPage {
        current_state: row.current_state,
        article: row.article,
        ..WikiPage::empty(slug, row.title)
    };
    page.meta.aliases = row.aliases;
    page.meta.tags = row.tags;
    page.meta.entities = row.entities;
    page.meta.related = row.related;
    Ok(page)
}

struct Repositories(Vec<(String, LockedRepository)>);

impl Repositories {
    async fn lock(memory: &PostgresMemory, homes: &Homes) -> Result<Self, WikiStoreError> {
        let directories: BTreeSet<&str> =
            homes.values().map(|home| home.directory.as_str()).collect();
        let mut locked = Self(Vec::new());
        for directory in directories {
            let repository = match memory.wiki_home(directory) {
                Ok(path) => WikiRepository::for_store(path)
                    .lock()
                    .await
                    .map_err(WikiStoreError::from),
                Err(error) => Err(error),
            };
            match repository {
                Ok(repository) => locked.0.push((directory.to_owned(), repository)),
                Err(error) => return settle(Err(error), locked.release().await),
            }
        }
        Ok(locked)
    }

    fn at(&mut self, directory: &str) -> Result<&mut LockedRepository, WikiStoreError> {
        self.0
            .iter_mut()
            .find(|(held, _)| held == directory)
            .map(|(_, repository)| repository)
            .ok_or(WikiStoreError::DirectoryMissing)
    }

    async fn write(
        &mut self,
        page: &mut WikiPage,
        directory: &str,
        mut removed: Removed,
        message: &str,
        now: Timestamp,
    ) -> Result<String, WikiStoreError> {
        let beside = removed.remove(directory).unwrap_or_default();
        let git_sha = self
            .at(directory)?
            .fold(page, &beside, message, now)
            .await?;
        for (elsewhere, locals) in removed {
            self.at(&elsewhere)?.remove(&locals, message).await?;
        }
        Ok(git_sha)
    }

    async fn release(self) -> Result<(), WikiWriteError> {
        let mut released = Ok(());
        for (_, repository) in self.0 {
            released = released.and(repository.release().await);
        }
        released
    }
}

fn take_section(
    page: &mut WikiPage,
    row: &mut TextRow,
    file: Option<&mut WikiPage>,
    owner: &Topic,
) -> Result<(), WikiStoreError> {
    let mut parent = super::topic_pages::topic_page(page.clone())?;
    let (summary, article) = file.as_ref().map_or_else(
        || (row.current_state.clone(), row.article.clone()),
        |file| (file.current_state.clone(), file.article.clone()),
    );
    let mut child = TopicPage {
        summary,
        article,
        ..TopicPage::empty(parent.slug.clone(), &row.title)
    };
    parent.take_text(&mut child, owner.id, owner.name.as_str());
    super::topic_pages::set_sections(page, &parent.sections);
    row.current_state.clone_from(&child.summary);
    row.article.clone_from(&child.article);
    if let Some(file) = file {
        file.current_state = child.summary;
        file.article = child.article;
    }
    Ok(())
}

fn absorb(page: &mut WikiPage, slug: &str, row: TextRow, child: Option<WikiPage>, mark: FoldMark) {
    if let Some(child) = child {
        fold_page(page, child, mark);
        return;
    }
    if !page.meta.aliases.iter().any(|alias| alias == slug) {
        page.meta.aliases.push(slug.to_owned());
    }
    fold_text(
        &mut page.current_state,
        &mut page.article,
        FoldedTopic {
            slug: slug.to_owned(),
            title: row.title,
            current_state: row.current_state,
            article: row.article,
        },
        mark,
    );
}

async fn text(
    tx: &mut Transaction<'_, Postgres>,
    slug: &str,
) -> Result<Option<TextRow>, WikiStoreError> {
    sqlx::query_as::<_, TextRow>(
        "SELECT title, current_state, article, aliases, tags, entities, related
           FROM rh_wiki_topics WHERE slug = $1",
    )
    .bind(slug)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)
}

async fn retire(
    tx: &mut Transaction<'_, Postgres>,
    child: &str,
    parent: &str,
    now: Timestamp,
    live: &[uuid::Uuid],
) -> Result<(), WikiStoreError> {
    for statement in [
        "INSERT INTO rh_wiki_citations (topic_slug, summary_id, kind, note, cited_at, owner_user_id)
         SELECT $2, summary_id, kind, note, cited_at, owner_user_id
           FROM rh_wiki_citations WHERE topic_slug = $1 AND summary_id = ANY($3)
         ON CONFLICT DO NOTHING",
        "INSERT INTO rh_wiki_provenance
             (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id)
         SELECT $2, source_kind, source_id, conversation_id, git_sha, created_at, owner_user_id
           FROM rh_wiki_provenance WHERE topic_slug = $1 AND (source_kind <> 'summary' OR source_id = ANY($3))
         ON CONFLICT DO NOTHING",
    ] {
        sqlx::query(statement)
            .bind(child)
            .bind(parent)
            .bind(live)
            .execute(&mut **tx)
            .await
            .map_err(query_error)?;
    }
    sqlx::query("UPDATE rh_wiki_redirects SET to_slug = $2 WHERE to_slug = $1")
        .bind(child)
        .bind(parent)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    sqlx::query!("DELETE FROM rh_wiki_redirects WHERE from_slug = $1", parent)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    sqlx::query!(
        "WITH retired AS (
            DELETE FROM rh_wiki_topics WHERE slug = $1 RETURNING owner_user_id
         )
         INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         SELECT $1, $2, $3, coalesce(rh_scope_principal(), (SELECT owner_user_id FROM retired))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
        child,
        parent,
        crate::time::BindTime::bind_time(now) as _
    )
    .execute(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(())
}

fn count(value: i64) -> Result<u64, WikiStoreError> {
    u64::try_from(value).map_err(|error| WikiStoreError::Query(Box::new(error)))
}

#[cfg(test)]
#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn folding_adopts_a_legacy_redirect(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
         VALUES ('child', 'Child', 'alice'), ('parent', 'Parent', 'alice');
         INSERT INTO rh_wiki_redirects (from_slug, to_slug) VALUES ('child', 'parent')",
    )
    .execute(&mut *tx)
    .await?;
    retire(&mut tx, "child", "parent", Timestamp::now(), &[]).await?;
    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_user_id FROM rh_wiki_redirects WHERE from_slug = 'child'")
            .fetch_one(&mut *tx)
            .await?;
    assert_eq!(owner.as_deref(), Some("alice"));
    Ok(())
}

#[cfg(test)]
#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn folding_a_child_with_no_page_row_still_redirects(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('parent', 'Parent', 'alice')",
    )
    .execute(&mut *tx)
    .await?;
    retire(&mut tx, "child", "parent", Timestamp::now(), &[]).await?;
    let target: Option<String> =
        sqlx::query_scalar("SELECT to_slug FROM rh_wiki_redirects WHERE from_slug = 'child'")
            .fetch_optional(&mut *tx)
            .await?;
    assert_eq!(target.as_deref(), Some("parent"));
    Ok(())
}
