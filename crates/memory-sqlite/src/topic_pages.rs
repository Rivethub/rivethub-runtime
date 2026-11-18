use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::topics::PageChange;
use kernel::topics::PageEdit;
use kernel::topics::PageSection;
use kernel::topics::TopicId;
use kernel::topics::TopicPage;
use kernel::topics::TopicPageStore;
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::WikiConsolidateStore;
use kernel::wiki::consolidate::WikiFold;
use kernel::wiki::page::ArticleSection;
use kernel::wiki::page::FoldMark;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::bound::bound_parts;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use serde::Deserialize;
use serde::Serialize;

use crate::SqliteStore;
use crate::time;
use crate::wiki_builder::blocking;
use crate::wiki_builder::query;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredSection {
    topic: Option<TopicId>,
    heading: String,
    body: String,
}

#[async_trait]
impl TopicPageStore for SqliteStore {
    async fn topic_page(&self, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError> {
        let slug = slug.clone();
        blocking(self, move |conn| read(conn, &slug)).await
    }

    async fn page_taken(&self, slug: &WikiSlug) -> Result<bool, WikiStoreError> {
        let slug = slug.as_ref().to_owned();
        blocking(self, move |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM ros_wiki_topics WHERE slug = ?1)
                     OR EXISTS (SELECT 1 FROM ros_wiki_redirects WHERE from_slug = ?1)",
                params![slug],
                |row| row.get(0),
            )
            .map_err(query)
        })
        .await
    }

    async fn save_topic_page(
        &self,
        topic: TopicId,
        page: &TopicPage,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let page = page.clone();
        let origin = kernel::wiki::origin::NoteOrigin::current();
        blocking(self, move |conn| {
            let tx = crate::summary_replace::immediate(conn).map_err(query)?;
            if let Some(id) = origin {
                crate::summary_replace::shared(&tx, id)
                    .map_err(crate::wiki_builder::fence_error)?;
            }
            save(&tx, topic, &page, &time::stamp(now))?;
            tx.commit().map_err(query)
        })
        .await
    }

    async fn edit_page(
        &self,
        slug: &WikiSlug,
        edit: &PageEdit,
        now: DateTime<Utc>,
    ) -> Result<bool, WikiStoreError> {
        let (slug, edit) = (slug.clone(), edit.clone());
        let origin = kernel::wiki::origin::NoteOrigin::current();
        blocking(self, move |conn| {
            let tx = crate::summary_replace::immediate(conn).map_err(query)?;
            if let Some(id) = origin {
                crate::summary_replace::shared(&tx, id)
                    .map_err(crate::wiki_builder::fence_error)?;
            }
            let owner = tx
                .query_row(
                    "SELECT topic_id FROM rh_topic_pages WHERE page_slug = ?1",
                    params![slug.as_ref()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(query)?;
            let (Some(owner), Some(mut page)) = (owner, read(&tx, &slug)?) else {
                return Ok(false);
            };
            let topic = owner
                .parse::<TopicId>()
                .map_err(|error| WikiStoreError::Query(Box::new(error)))?;
            if let Some(text) = &edit.current_state {
                text.trim().clone_into(&mut page.summary);
            }
            if let Some(text) = &edit.article {
                text.trim().clone_into(&mut page.article);
            }
            save(&tx, topic, &page, &time::stamp(now))?;
            tx.commit().map_err(query)?;
            Ok(true)
        })
        .await
    }

    async fn change_page(
        &self,
        root: TopicId,
        slug: &WikiSlug,
        change: &PageChange<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let origin = kernel::wiki::origin::NoteOrigin::current();
        for _ in 0..CHANGE_ATTEMPTS {
            let before = self
                .topic_page(slug)
                .await?
                .ok_or_else(|| WikiStoreError::TopicMissing(slug.as_ref().to_owned()))?;
            let mut after = before.clone();
            change(&mut after);
            if after == before {
                return Ok(());
            }
            let swapped = blocking(self, move |conn| {
                let tx = crate::summary_replace::immediate(conn).map_err(query)?;
                if let Some(id) = origin {
                    crate::summary_replace::shared(&tx, id)
                        .map_err(crate::wiki_builder::fence_error)?;
                }
                if read(&tx, &before.slug)?.as_ref() != Some(&before) {
                    return Ok(false);
                }
                save(&tx, root, &after, &time::stamp(now))?;
                tx.commit().map_err(query)?;
                Ok(true)
            })
            .await?;
            if swapped {
                return Ok(());
            }
        }
        Err(WikiStoreError::InvalidPatch(format!(
            "topic page {} kept changing under the update",
            slug.as_ref()
        )))
    }

    async fn redirect_page(
        &self,
        from: &WikiSlug,
        to: &WikiSlug,
        now: DateTime<Utc>,
    ) -> Result<(), WikiStoreError> {
        let (from, to) = (from.as_ref().to_owned(), to.as_ref().to_owned());
        blocking(self, move |conn| {
            conn.execute(
                "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at)
                 SELECT ?1, ?2, ?3
                  WHERE NOT EXISTS (SELECT 1 FROM ros_wiki_topics WHERE slug = ?1)
                 ON CONFLICT(from_slug) DO UPDATE SET to_slug = excluded.to_slug
                  WHERE ros_wiki_redirects.to_slug <> excluded.to_slug",
                params![from, to, time::stamp(now)],
            )
            .map_err(query)?;
            Ok(())
        })
        .await
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
        let slug = slug.as_ref().to_owned();
        blocking(self, move |conn| {
            for statement in [
                "DELETE FROM ros_wiki_citations WHERE topic_slug = ?1",
                "DELETE FROM ros_wiki_provenance WHERE topic_slug = ?1",
                "DELETE FROM ros_wiki_redirects WHERE to_slug = ?1",
                "DELETE FROM ros_wiki_topics WHERE slug = ?1",
            ] {
                conn.execute(statement, params![slug]).map_err(query)?;
            }
            Ok(())
        })
        .await
    }
}

fn read(conn: &Connection, slug: &WikiSlug) -> Result<Option<TopicPage>, WikiStoreError> {
    let row = conn
        .query_row(
            "SELECT title, current_state, article, (SELECT sections FROM rh_topic_pages
                                                     WHERE page_slug = ros_wiki_topics.slug)
               FROM ros_wiki_topics WHERE slug = ?1",
            params![slug.as_ref()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(query)?;
    let Some((title, summary, article, sections)) = row else {
        return Ok(None);
    };
    Ok(Some(TopicPage {
        slug: slug.clone(),
        title,
        summary,
        article,
        sections: decode(sections.as_deref())?
            .into_iter()
            .map(|section| PageSection {
                topic: section.topic,
                heading: section.heading,
                body: section.body,
            })
            .collect(),
    }))
}

fn save(
    conn: &Connection,
    topic: TopicId,
    page: &TopicPage,
    at: &str,
) -> Result<(), WikiStoreError> {
    let page = &bounded(page);
    let lists = conn
        .query_row(
            "SELECT aliases, tags, entities FROM ros_wiki_topics WHERE slug = ?1",
            params![page.slug.as_ref()],
            |row| Ok([row.get::<_, String>(0)?, row.get(1)?, row.get(2)?]),
        )
        .optional()
        .map_err(query)?;
    let words: Vec<String> = match lists {
        Some(lists) => lists
            .iter()
            .map(|list| Ok(decode_list(list)?.join(" ")))
            .collect::<Result<_, WikiStoreError>>()?,
        None => vec![String::new(); 3],
    };
    let search = format!(
        "{}\n{}\n{}\n{}",
        page.title,
        page.summary,
        words.join("\n"),
        page.article
    );
    conn.execute(
        "INSERT INTO ros_wiki_topics (slug, title, current_state, article, search_text, created_at,
                                      updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(slug) DO UPDATE SET title = excluded.title,
            current_state = excluded.current_state, article = excluded.article,
            search_text = excluded.search_text, updated_at = excluded.updated_at,
            embedding = NULL, embed_status = NULL, embed_failures = 0, embed_error = NULL",
        params![
            page.slug.as_ref(),
            page.title,
            page.summary,
            page.article,
            search,
            at
        ],
    )
    .map_err(query)?;
    conn.execute(
        "DELETE FROM rh_topic_pages WHERE topic_id = ?1 OR page_slug = ?2",
        params![topic.to_string(), page.slug.as_ref()],
    )
    .map_err(query)?;
    let sections: Vec<StoredSection> = page
        .sections
        .iter()
        .map(|section| StoredSection {
            topic: section.topic,
            heading: section.heading.clone(),
            body: section.body.clone(),
        })
        .collect();
    conn.execute(
        "INSERT INTO rh_topic_pages (topic_id, page_slug, sections) VALUES (?1, ?2, ?3)",
        params![topic.to_string(), page.slug.as_ref(), encode(&sections)?],
    )
    .map_err(query)?;
    Ok(())
}

pub(crate) fn carry(
    conn: &Connection,
    child: &str,
    parent: &str,
    article: &mut String,
) -> Result<(), WikiStoreError> {
    let Some(child_sections) = bound_sections(conn, child)? else {
        return Ok(());
    };
    let Some(mut kept) = bound_sections(conn, parent)? else {
        for section in child_sections {
            let entry = format!("#### {}\n\n{}", section.heading, section.body.trim());
            if !article.contains(&entry) {
                let joined = format!("{}\n\n{entry}", article.trim_end());
                joined.trim().clone_into(article);
            }
        }
        return Ok(());
    };
    for section in child_sections {
        let known = kept.iter().any(|existing| match section.topic {
            Some(topic) => existing.topic == Some(topic),
            None => existing.topic.is_none() && existing.heading == section.heading,
        });
        if !known {
            kept.push(section);
        }
    }
    conn.execute(
        "UPDATE rh_topic_pages SET sections = ?2 WHERE page_slug = ?1",
        params![parent, encode(&kept)?],
    )
    .map_err(query)?;
    Ok(())
}

pub(crate) fn rendered(sections: Option<&str>) -> Result<String, serde_json::Error> {
    let sections: Vec<StoredSection> = match sections {
        Some(text) => serde_json::from_str(text)?,
        None => Vec::new(),
    };
    Ok(sections
        .iter()
        .map(|section| format!("\n## {}\n\n{}\n", section.heading, section.body.trim()))
        .collect::<Vec<_>>()
        .concat())
}

fn bound_sections(
    conn: &Connection,
    slug: &str,
) -> Result<Option<Vec<StoredSection>>, WikiStoreError> {
    conn.query_row(
        "SELECT sections FROM rh_topic_pages WHERE page_slug = ?1",
        params![slug],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(query)?
    .map(|text| decode(Some(&text)))
    .transpose()
}

fn decode(text: Option<&str>) -> Result<Vec<StoredSection>, WikiStoreError> {
    match text {
        Some(text) => {
            serde_json::from_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
        }
        None => Ok(Vec::new()),
    }
}

fn decode_list(text: &str) -> Result<Vec<String>, WikiStoreError> {
    serde_json::from_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn encode(sections: &[StoredSection]) -> Result<String, WikiStoreError> {
    serde_json::to_string(sections).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

const CHANGE_ATTEMPTS: usize = 8;

fn bounded(page: &TopicPage) -> TopicPage {
    let mut page = page.clone();
    let mut sections: Vec<ArticleSection> = page
        .sections
        .iter()
        .map(|section| ArticleSection {
            heading: section.heading.clone(),
            body: section.body.clone(),
        })
        .collect();
    let owned: Vec<String> = page
        .sections
        .iter()
        .filter(|section| section.topic.is_some())
        .map(|section| section.heading.clone())
        .collect();
    let archived = bound_parts(&mut page.summary, &mut page.article, &mut sections, &owned);
    if !archived.is_empty() {
        tracing::info!(
            "wiki page {} trimmed to its budget: {} block(s) left out",
            page.slug.as_ref(),
            archived.len()
        );
    }
    for (section, fitted) in page.sections.iter_mut().zip(sections) {
        section.body = fitted.body;
    }
    page.sections
        .retain(|section| section.topic.is_some() || !section.body.trim().is_empty());
    page
}
