use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::consolidate::WikiTopicStats;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::FoldedTopic;
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
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, WikiStoreError> {
        let fold = fold.clone();
        blocking(self, move |conn| fold_all(conn, &fold, now)).await
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
    now: DateTime<Utc>,
) -> Result<Vec<String>, WikiStoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(query)?;
    let mut parent = load(&tx, &fold.parent)?
        .ok_or_else(|| WikiStoreError::TopicMissing(fold.parent.clone()))?;
    let mut folded = Vec::new();
    for slug in &fold.children {
        if let Some(child) = load(&tx, slug)? {
            absorb(&mut parent, child, fold.mark);
            crate::topic_pages::carry(&tx, slug, &fold.parent, &mut parent.article)?;
            folded.push(slug.clone());
        }
    }
    if folded.is_empty() {
        tx.commit().map_err(query)?;
        return Ok(folded);
    }
    let at = crate::time::stamp(now);
    store_parent(&tx, &parent, &at)?;
    for child in &folded {
        retire(&tx, child, &parent.slug, &at)?;
    }
    tx.commit().map_err(query)?;
    Ok(folded)
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
                    history_count
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
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .map_err(query)?;
    let Some((slug, title, [aliases, tags, entities, related], current_state, article, history)) =
        row
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
        history_count: history,
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
    conn.execute(
        "UPDATE ros_wiki_topics SET aliases = ?2, tags = ?3, entities = ?4, related = ?5,
            current_state = ?6, article = ?7, search_text = ?8, history_count = ?9,
            updated_at = ?10, embedding = NULL, embed_status = NULL, embed_failures = 0,
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
         SELECT ?2, summary_id, kind, note, cited_at FROM ros_wiki_citations
          WHERE topic_slug = ?1
         ON CONFLICT DO NOTHING",
        params![child, parent],
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_provenance
            (topic_slug, source_kind, source_id, conversation_id, git_sha, created_at)
         SELECT ?2, source_kind, source_id, conversation_id, git_sha, created_at
           FROM ros_wiki_provenance WHERE topic_slug = ?1
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
