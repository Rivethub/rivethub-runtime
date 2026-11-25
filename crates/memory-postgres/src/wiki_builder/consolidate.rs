use async_trait::async_trait;
use jiff::Timestamp;
use kernel::topics::{Topic, TopicPage};
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::{WikiConsolidateStore, WikiFold, WikiTopicStats};
use kernel::wiki::page::{FoldMark, FoldedTopic, WikiPage, WikiSlug, fold_page, fold_text};
use sqlx::{PgConnection, Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};

use super::{lock_slug, query_error, settle, topics};
use crate::PostgresMemory;
use crate::wiki_scope::{LegacyOwner, PageHome, scope_named, with_homes};
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
    homed: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct HomeRow {
    slug: String,
    #[sqlx(flatten)]
    home: PageHome,
    homed: bool,
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
        let legacy = self.legacy_owner().await?;
        let rows = sqlx::query_as::<_, StatsRow>(with_homes(
            "SELECT t.slug, t.title, t.history_count, t.created_at, t.topic_id IS NOT NULL AS bound,
                    (SELECT count(*) FROM rh_wiki_citations c WHERE c.topic_slug = t.slug) AS citations,
                    h.homed
               FROM rh_wiki_topics t JOIN homes h ON h.slug = t.slug
              WHERE h.directory = ''
              ORDER BY t.slug",
        ))
        .bind(legacy.owner())
        .bind(legacy.open())
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        let (rows, unhomed): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.homed);
        if !unhomed.is_empty() {
            let skipped: Vec<&str> = unhomed.iter().map(|row| row.slug.as_str()).collect();
            tracing::warn!(
                "wiki consolidate skipped {}: no verified home in the primary wiki",
                skipped.join(", ")
            );
        }
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
        let directory = &home_of(&homes, &fold.parent)?.directory;
        let mut repository = WikiRepository::for_store(self.wiki_home(directory)?)
            .lock()
            .await?;
        let folded = self
            .fold_pages(&mut repository, fold, &homes, section, now)
            .await;
        settle(folded, repository.release().await)
    }

    async fn fold_homes(&self, fold: &WikiFold) -> Result<Homes, WikiStoreError> {
        let legacy = self.legacy_owner().await?;
        let mut connection = self.pool.acquire().await.map_err(query_error)?;
        read_homes(&mut connection, fold, &legacy).await
    }

    async fn lock_homes(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        fold: &WikiFold,
        homes: &Homes,
    ) -> Result<(), WikiStoreError> {
        for key in fold_keys(fold) {
            lock_slug(tx, key).await?;
        }
        let legacy = self.held_legacy_owner(tx).await?;
        if read_homes(tx, fold, &legacy).await? != *homes {
            return Err(WikiStoreError::InvalidPatch(format!(
                "the pages of the {} fold changed while it waited for them",
                fold.parent
            )));
        }
        Ok(())
    }

    async fn fold_pages(
        &self,
        repository: &mut LockedRepository,
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
        self.lock_homes(&mut tx, fold, homes).await?;
        let parent = home_of(homes, &fold.parent)?;
        let row = text(&mut tx, &fold.parent)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(fold.parent.clone()))?;
        let mut page = match repository.read(&parent.local).await? {
            Some(page) => page,
            None => indexed_page(&parent.local, row)?,
        };
        let mut removed = Vec::new();
        let mut folded = Vec::new();
        for child in fold.children.iter().filter(|child| **child != fold.parent) {
            let Some(home) = homes.get(child) else {
                continue;
            };
            let Some(mut row) = text(&mut tx, child).await? else {
                continue;
            };
            let mut file = repository.read(&home.local).await?;
            if let Some(owner) = section {
                take_section(&mut page, &mut row, file.as_mut(), owner)?;
            }
            if file.is_some() {
                removed.push(home.local.clone());
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
        let git_sha = repository.fold(&mut page, &removed, &message, now).await?;
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

async fn read_homes(
    conn: &mut PgConnection,
    fold: &WikiFold,
    legacy: &LegacyOwner,
) -> Result<Homes, WikiStoreError> {
    let keys: Vec<&str> = fold_keys(fold).into_iter().collect();
    let rows = sqlx::query_as::<_, HomeRow>(with_homes(
        "SELECT slug, scope, directory, local, homed FROM homes WHERE slug = ANY($3)",
    ))
    .bind(legacy.owner())
    .bind(legacy.open())
    .bind(&keys)
    .fetch_all(&mut *conn)
    .await
    .map_err(query_error)?;
    let (verified, strays): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.homed);
    let homes: Homes = verified
        .into_iter()
        .map(|row| (row.slug, row.home))
        .collect();
    let parent = home_of(&homes, &fold.parent)?;
    if let Some(stray) = strays
        .iter()
        .map(|row| &row.slug)
        .chain(
            homes
                .iter()
                .filter(|(_, home)| home.directory != parent.directory)
                .map(|(key, _)| key),
        )
        .next()
    {
        return Err(WikiStoreError::InvalidPatch(format!(
            "wiki page {stray} is not a page of the wiki of {}; a fold cannot span wiki homes",
            fold.parent
        )));
    }
    Ok(homes)
}

fn fold_keys(fold: &WikiFold) -> BTreeSet<&str> {
    std::iter::once(&fold.parent)
        .chain(&fold.children)
        .map(String::as_str)
        .collect()
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
        "SELECT title, current_state, article, aliases, tags, entities,
                ARRAY(SELECT coalesce((SELECT k.local_slug FROM rh_wiki_topics k WHERE k.slug = r.slug AND k.scope = t.scope), r.slug)
                        FROM unnest(t.related) WITH ORDINALITY AS r(slug, n) ORDER BY r.n) AS related
           FROM rh_wiki_topics t WHERE t.slug = $1",
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
    let owner = sqlx::query_scalar::<_, Option<String>>(
        "DELETE FROM rh_wiki_topics WHERE slug = $1 RETURNING owner_user_id",
    )
    .bind(child)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)?
    .ok_or_else(|| {
        WikiStoreError::InvalidPatch(format!("wiki page {child} is not yours to fold away"))
    })?;
    sqlx::query(
        "INSERT INTO rh_wiki_redirects (from_slug, to_slug, created_at, owner_user_id)
         VALUES ($1, $2, $3, coalesce(rh_scope_principal(), $4))
         ON CONFLICT (from_slug) DO UPDATE SET to_slug = EXCLUDED.to_slug,
             owner_user_id = coalesce(rh_wiki_redirects.owner_user_id, EXCLUDED.owner_user_id)",
    )
    .bind(child)
    .bind(parent)
    .bind(crate::time::BindTime::bind_time(now))
    .bind(owner)
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
async fn folding_a_child_with_no_page_row_to_delete_is_refused(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(
        "INSERT INTO rh_wiki_topics (slug, title, owner_user_id) VALUES ('parent', 'Parent', 'alice')",
    )
    .execute(&mut *tx)
    .await?;
    let retired = retire(&mut tx, "child", "parent", Timestamp::now(), &[]).await;
    assert!(
        matches!(&retired, Err(WikiStoreError::InvalidPatch(_))),
        "got {retired:?}"
    );
    let target: Option<String> =
        sqlx::query_scalar("SELECT to_slug FROM rh_wiki_redirects WHERE from_slug = 'child'")
            .fetch_optional(&mut *tx)
            .await?;
    assert_eq!(
        target, None,
        "no redirect is written for a row that was not deleted"
    );
    Ok(())
}
