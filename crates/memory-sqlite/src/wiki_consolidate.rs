use jiff::Timestamp;
use std::collections::BTreeSet;

use async_trait::async_trait;
use kernel::topics::TopicPage;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::consolidate::WikiTopicStats;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::FoldedTopic;
use kernel::wiki::page::WikiHistoryEntry;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::fold_page;
use kernel::wiki::page::fold_text;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::TransactionBehavior;
use rusqlite::params;

use crate::SqliteStore;
use crate::wiki_builder::blocking;
use crate::wiki_builder::query;

struct Topic {
    slug: String,
    title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    related: Vec<String>,
    current_state: String,
    article: String,
    history: Vec<WikiHistoryEntry>,
    history_count: i64,
}

#[async_trait]
impl WikiConsolidateStore for SqliteStore {
    async fn wiki_topic_stats(&self) -> Result<Vec<WikiTopicStats>, WikiStoreError> {
        blocking(self, |conn| topics(conn)).await
    }

    async fn fold_wiki(
        &self,
        fold: &WikiFold,
        now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let fold = fold.clone();
        let Some(directory) = self.wiki_directory().map(std::path::Path::to_path_buf) else {
            return blocking(self, move |conn| fold_all(conn, &fold, now)).await;
        };
        fold_files(self, fold, now, directory).await
    }
}

fn topics(conn: &Connection) -> Result<Vec<WikiTopicStats>, WikiStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT t.slug, t.title, t.history_count, t.created_at,
                    (SELECT count(*) FROM ros_wiki_citations c WHERE c.topic_slug = t.slug),
                    EXISTS (SELECT 1 FROM rh_topic_pages p
                             WHERE p.page_slug = t.slug AND p.topic_id IS NOT NULL)
               FROM ros_wiki_topics t ORDER BY t.slug",
        )
        .map_err(query)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(query)?;
    let mut topics = Vec::new();
    for row in rows {
        let (slug, title, history, created_at, citations, bound) = row.map_err(query)?;
        topics.push(WikiTopicStats {
            slug,
            title,
            citations: u64::try_from(citations)
                .map_err(|source| WikiStoreError::Query(Box::new(source)))?,
            history: u64::try_from(history)
                .map_err(|source| WikiStoreError::Query(Box::new(source)))?,
            created_at: crate::time::stored(&created_at, "ros_wiki_topics.created_at")
                .map_err(|source| WikiStoreError::Query(Box::new(source)))?,
            bound,
        });
    }
    Ok(topics)
}

fn fold_all(
    conn: &mut Connection,
    fold: &WikiFold,
    now: Timestamp,
) -> Result<Vec<String>, WikiStoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(query)?;
    let folded = fold_rows(&tx, fold, now)?;
    tx.commit().map_err(query)?;
    Ok(folded)
}

fn fold_rows(
    conn: &Connection,
    fold: &WikiFold,
    now: Timestamp,
) -> Result<Vec<String>, WikiStoreError> {
    let mut parent = load(conn, &fold.parent)?
        .ok_or_else(|| WikiStoreError::TopicMissing(fold.parent.clone()))?;
    let mut folded = Vec::new();
    for slug in &fold.children {
        if let Some(child) = load(conn, slug)? {
            absorb(&mut parent, child, fold.mark);
            crate::topic_pages::carry(conn, slug, &fold.parent, &mut parent.article)?;
            folded.push(slug.clone());
        }
    }
    if folded.is_empty() {
        return Ok(folded);
    }
    let at = crate::time::stamp(now);
    store_parent(conn, &parent, &at)?;
    for child in &folded {
        retire(conn, child, &parent.slug, &at)?;
    }
    Ok(folded)
}

async fn fold_files(
    store: &crate::SqliteStore,
    fold: WikiFold,
    now: Timestamp,
    directory: std::path::PathBuf,
) -> Result<Vec<String>, WikiStoreError> {
    let path = crate::page_files::database_path(store).await?;
    let locked = crate::page_files::Files::wiki(&directory).lock().await?;
    crate::page_files::blocking_locked(locked, move |locked| fold_held(locked, &path, &fold, now))
        .await
}

fn fold_held(
    locked: &mut wiki_files::wiki_writer::LockedRepository,
    path: &std::path::Path,
    fold: &WikiFold,
    now: Timestamp,
) -> Result<Vec<String>, WikiStoreError> {
    let mut held = crate::page_files::HeldTx::open(path)?;
    let tx = held.immediate()?;
    let loaded = child_rows(&tx, fold)?;
    let outcome =
        tokio::runtime::Handle::current().block_on(fold_locked(locked, &tx, fold, &loaded, now))?;
    let folded = write_outcome(&tx, fold, outcome, now)?;
    tx.commit().map_err(query)?;
    Ok(folded)
}

fn write_outcome(
    conn: &Connection,
    fold: &WikiFold,
    outcome: FoldOutcome,
    now: Timestamp,
) -> Result<Vec<String>, WikiStoreError> {
    crate::page_files::pause_before_row();
    match outcome {
        FoldOutcome::Unchanged => Ok(Vec::new()),
        FoldOutcome::NoFile => fold_rows(conn, fold, now),
        FoldOutcome::Wrote { page, sha, folded } => {
            save_fold(conn, &fold.parent, &page, &sha, &folded, now)
        }
    }
}

struct FoldRows {
    parent: TopicPage,
    rows: Vec<Topic>,
    pages: Vec<TopicPage>,
}

fn child_rows(conn: &Connection, fold: &WikiFold) -> Result<FoldRows, WikiStoreError> {
    let parent = page_row(conn, &fold.parent)?;
    let mut rows = Vec::new();
    let mut pages = Vec::new();
    for child in fold.children.iter().filter(|child| **child != fold.parent) {
        if let Some(row) = load(conn, child)? {
            if let Some(page) = crate::topic_pages::read(conn, &slug_of(child)?)? {
                pages.push(page);
            }
            rows.push(row);
        }
    }
    Ok(FoldRows {
        parent,
        rows,
        pages,
    })
}

fn page_row(conn: &Connection, slug: &str) -> Result<TopicPage, WikiStoreError> {
    crate::topic_pages::read(conn, &slug_of(slug)?)?
        .ok_or_else(|| WikiStoreError::TopicMissing(slug.to_owned()))
}

fn slug_of(slug: &str) -> Result<WikiSlug, WikiStoreError> {
    WikiSlug::try_from(slug.to_owned())
        .map_err(|error| WikiStoreError::InvalidPatch(error.to_string()))
}

enum FoldOutcome {
    NoFile,
    Unchanged,
    Wrote {
        page: Box<WikiPage>,
        sha: String,
        folded: Vec<String>,
    },
}

async fn fold_locked(
    locked: &mut wiki_files::wiki_writer::LockedRepository,
    conn: &rusqlite::Connection,
    fold: &WikiFold,
    loaded: &FoldRows,
    now: Timestamp,
) -> Result<FoldOutcome, WikiStoreError> {
    crate::wiki_materialize::record_stored(locked, conn, &loaded.parent.slug, now).await?;
    for page in &loaded.pages {
        crate::wiki_materialize::record_stored(locked, conn, &page.slug, now).await?;
    }
    let Some(mut page) = locked
        .read(&fold.parent)
        .await
        .map_err(WikiStoreError::from)?
    else {
        return Ok(FoldOutcome::NoFile);
    };
    let mut folded = Vec::new();
    for child in fold.children.iter().filter(|child| **child != fold.parent) {
        let Some(row) = loaded.rows.iter().find(|row| row.slug == *child) else {
            continue;
        };
        let filed = locked.read(child).await.map_err(WikiStoreError::from)?;
        absorb_file(&mut page, child, row, filed, fold.mark);
        folded.push(child.clone());
    }
    if folded.is_empty() {
        return Ok(FoldOutcome::Unchanged);
    }
    let message = format!(
        "wiki({}): consolidate {}\n\nPipeline: wiki-v7",
        fold.parent,
        folded.join(", ")
    );
    let sha = locked
        .fold(&mut page, &folded, &message, now)
        .await
        .map_err(WikiStoreError::from)?;
    Ok(FoldOutcome::Wrote {
        page: Box::new(page),
        sha,
        folded,
    })
}

fn save_fold(
    conn: &Connection,
    parent: &str,
    page: &WikiPage,
    sha: &str,
    folded: &[String],
    now: Timestamp,
) -> Result<Vec<String>, WikiStoreError> {
    crate::wiki_page_files::upsert(conn, page, sha, now)?;
    let at = crate::time::stamp(now);
    for child in folded {
        retire(conn, child, parent, &at)?;
    }
    Ok(folded.to_vec())
}

fn absorb_file(
    page: &mut WikiPage,
    slug: &str,
    row: &Topic,
    child: Option<WikiPage>,
    mark: FoldMark,
) {
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
            title: row.title.clone(),
            current_state: row.current_state.clone(),
            article: row.article.clone(),
        },
        mark,
    );
}

fn absorb(parent: &mut Topic, child: Topic, mark: FoldMark) {
    let mut aliases: BTreeSet<String> = std::mem::take(&mut parent.aliases).into_iter().collect();
    aliases.insert(child.slug.clone());
    aliases.extend(child.aliases);
    aliases.remove(&parent.slug);
    parent.aliases = aliases.into_iter().collect();
    merge_unique(&mut parent.tags, child.tags);
    merge_unique(&mut parent.entities, child.entities);
    let related: Vec<String> = child
        .related
        .into_iter()
        .filter(|slug| *slug != parent.slug)
        .collect();
    merge_unique(&mut parent.related, related);
    parent.history_count += child.history_count;
    crate::wiki_history::merge(&mut parent.history, child.history);
    fold_text(
        &mut parent.current_state,
        &mut parent.article,
        FoldedTopic {
            slug: child.slug,
            title: child.title,
            current_state: child.current_state,
            article: child.article,
        },
        mark,
    );
}

fn merge_unique(existing: &mut Vec<String>, additions: Vec<String>) {
    for item in additions {
        if !item.is_empty() && !existing.contains(&item) {
            existing.push(item);
        }
    }
}

fn load(conn: &Connection, slug: &str) -> Result<Option<Topic>, WikiStoreError> {
    let row = conn
        .query_row(
            "SELECT slug, title, aliases, tags, entities, related, current_state, article,
                    history, history_count
               FROM ros_wiki_topics WHERE slug = ?1",
            params![slug],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    [
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ],
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()
        .map_err(query)?;
    let Some((
        slug,
        title,
        [aliases, tags, entities, related],
        current_state,
        article,
        history,
        history_count,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(Topic {
        slug,
        title,
        aliases: list(&aliases)?,
        tags: list(&tags)?,
        entities: list(&entities)?,
        related: list(&related)?,
        current_state,
        article,
        history: serde_json::from_str(&history)
            .map_err(|source| WikiStoreError::Query(Box::new(source)))?,
        history_count,
    }))
}

fn store_parent(conn: &Connection, topic: &Topic, at: &str) -> Result<(), WikiStoreError> {
    let search = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        topic.title,
        topic.current_state,
        topic.aliases.join(" "),
        topic.tags.join(" "),
        topic.entities.join(" "),
        topic.article
    );
    let mut kept = topic.history.clone();
    crate::wiki_history::cap(&mut kept);
    conn.execute(
        "UPDATE ros_wiki_topics SET aliases = ?2, tags = ?3, entities = ?4, related = ?5,
            current_state = ?6, article = ?7, history = ?8, search_text = ?9, history_count = ?10,
            updated_at = ?11, embedding = NULL, embed_status = NULL, embed_failures = 0,
            embed_error = NULL
         WHERE slug = ?1",
        params![
            topic.slug,
            json(&topic.aliases)?,
            json(&topic.tags)?,
            json(&topic.entities)?,
            json(&topic.related)?,
            topic.current_state,
            topic.article,
            crate::wiki_history::encode(&kept)?,
            search,
            topic.history_count,
            at
        ],
    )
    .map_err(query)?;
    Ok(())
}

fn retire(conn: &Connection, child: &str, parent: &str, at: &str) -> Result<(), WikiStoreError> {
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         SELECT ?2, summary_id, kind, note, cited_at FROM ros_wiki_citations c
          WHERE topic_slug = ?1 AND EXISTS (SELECT 1 FROM ros_summaries s WHERE s.id = c.summary_id)
         ON CONFLICT DO NOTHING",
        params![child, parent],
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_provenance
            (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at)
         SELECT ?2, source_kind, source_id, conversation_id, git_sha, created_at
           FROM ros_wiki_provenance p WHERE topic_slug = ?1 AND (source_kind <> 'summary'
                    OR EXISTS (SELECT 1 FROM ros_summaries s WHERE s.id = p.source_id))
         ON CONFLICT DO NOTHING",
        params![child, parent],
    )
    .map_err(query)?;
    conn.execute(
        "UPDATE ros_wiki_redirects SET to_slug = ?2 WHERE to_slug = ?1",
        params![child, parent],
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(from_slug) DO UPDATE SET to_slug = excluded.to_slug",
        params![child, parent, at],
    )
    .map_err(query)?;
    conn.execute(
        "DELETE FROM ros_wiki_redirects WHERE from_slug = ?1",
        params![parent],
    )
    .map_err(query)?;
    conn.execute(
        "DELETE FROM ros_wiki_topics WHERE slug = ?1",
        params![child],
    )
    .map_err(query)?;
    Ok(())
}

fn list(text: &str) -> Result<Vec<String>, WikiStoreError> {
    serde_json::from_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn json(values: &[String]) -> Result<String, WikiStoreError> {
    serde_json::to_string(values).map_err(|source| WikiStoreError::Query(Box::new(source)))
}
