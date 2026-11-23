use jiff::Timestamp;
use std::collections::BTreeSet;
use wire::time::TimestampFormat as _;

use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::builder::WikiSummary;
use kernel::wiki::page::WikiAction;
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::bound::{MergeBase, rebase};
use kernel::wiki::page::lead::stable_summary;
use kernel::wiki::page::topic_identity;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use thiserror::Error;

use crate::time;

#[derive(Debug, Error)]
#[error("{0}")]
struct Note(String);

struct Existing {
    title: String,
    aliases: String,
    tags: String,
    entities: String,
    current_state: String,
    article: String,
    related: String,
}

pub(crate) fn apply(
    conn: &Connection,
    summary: &WikiSummary,
    patch: &WikiPatch,
    page: Option<&WikiSlug>,
    now: Timestamp,
) -> Result<Option<String>, WikiStoreError> {
    let slug = canonical(conn, patch, page)?;
    let aliases = alias_list(patch, &slug);
    let at = time::stamp(now);
    let existing = load(conn, &slug)?;
    if let Some(row) = existing.as_ref().filter(|_| slug != patch.slug.as_ref()) {
        note(conn, summary, patch, &slug, row, now)?;
        return Ok(Some(slug));
    }
    let article = stored_article(patch.article.as_deref(), existing.as_ref());
    let related = stored_related(patch.related.as_ref(), existing.as_ref())?;
    let aliases_json = json(&aliases)?;
    let tags_json = json(&patch.tags)?;
    let entities_json = json(&patch.entities)?;
    let state = stabilized(existing.as_ref(), &patch.summary);
    let search = search_text(
        &patch.title,
        &state,
        [aliases.as_slice(), &patch.tags, &patch.entities],
        patch
            .article
            .as_deref()
            .or_else(|| existing.as_ref().map(|row| row.article.as_str())),
    );
    let changed = write_topic(
        conn,
        &slug,
        patch,
        &TopicWrite {
            aliases: aliases_json,
            tags: tags_json,
            entities: entities_json,
            summary: state,
            article,
            related,
            search,
            history: i64::from(patch.history_entry.is_some()),
            at: at.clone(),
        },
        existing.is_some(),
    )?;
    redirects(conn, &slug, &aliases, &at)?;
    provenance(
        conn,
        &slug,
        summary,
        patch
            .history_entry
            .as_ref()
            .map(|entry| entry.body.as_str()),
        &at,
    )?;
    Ok(changed.then_some(slug))
}

pub(crate) fn cite(
    conn: &Connection,
    slug: &str,
    summary: &WikiSummary,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    provenance(conn, slug, summary, None, &time::stamp(now))
}

struct TopicWrite {
    aliases: String,
    tags: String,
    entities: String,
    summary: String,
    article: String,
    related: String,
    search: String,
    history: i64,
    at: String,
}

fn stabilized(existing: Option<&Existing>, proposed: &str) -> String {
    match existing {
        Some(row) if !row.current_state.trim().is_empty() => {
            stable_summary(&row.current_state, proposed)
        }
        Some(_) | None => proposed.to_owned(),
    }
}

fn write_topic(
    conn: &Connection,
    slug: &str,
    patch: &WikiPatch,
    write: &TopicWrite,
    exists: bool,
) -> Result<bool, WikiStoreError> {
    if exists && unchanged(conn, slug, patch, write)? {
        return Ok(false);
    }
    let prior = if exists {
        crate::wiki_history::load(conn, slug)?
    } else {
        Vec::new()
    };
    let history = crate::wiki_history::encode(&crate::wiki_history::with_entry(
        prior,
        patch.history_entry.as_ref(),
    ))?;
    let rows = if exists {
        update_topic(conn, slug, patch, write, &history)?
    } else {
        insert_topic(conn, slug, patch, write, &history)?
    };
    if rows == 1 {
        Ok(true)
    } else {
        Err(WikiStoreError::Query(Box::new(Note(format!(
            "wiki topic {slug} was not written"
        )))))
    }
}

fn unchanged(
    conn: &Connection,
    slug: &str,
    patch: &WikiPatch,
    write: &TopicWrite,
) -> Result<bool, WikiStoreError> {
    let Some(row) = load(conn, slug)? else {
        return Ok(false);
    };
    if let Some(entry) = &patch.history_entry
        && !crate::wiki_history::load(conn, slug)?.contains(entry)
    {
        return Ok(false);
    }
    let state_matches = row.current_state == write.summary;
    Ok(row.title == patch.title
        && row.aliases == write.aliases
        && row.tags == write.tags
        && row.entities == write.entities
        && row.article == write.article
        && row.related == write.related
        && state_matches)
}

fn insert_topic(
    conn: &Connection,
    slug: &str,
    patch: &WikiPatch,
    write: &TopicWrite,
    history: &str,
) -> Result<usize, WikiStoreError> {
    conn.execute(
        "INSERT INTO ros_wiki_topics (
            slug, title, aliases, tags, entities, related, current_state, article, history,
            search_text, history_count, last_verified_at, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?12)",
        params![
            slug,
            patch.title,
            write.aliases,
            write.tags,
            write.entities,
            write.related,
            write.summary,
            write.article,
            history,
            write.search,
            write.history,
            write.at
        ],
    )
    .map_err(query)
}

fn update_topic(
    conn: &Connection,
    slug: &str,
    patch: &WikiPatch,
    write: &TopicWrite,
    history: &str,
) -> Result<usize, WikiStoreError> {
    conn.execute(
        "UPDATE ros_wiki_topics SET title = ?2, aliases = ?3, tags = ?4, entities = ?5,
            related = ?6, current_state = ?7, article = ?8, history = ?9, search_text = ?10,
            history_count = history_count + ?11, last_verified_at = ?12, updated_at = ?12,
            embedding = NULL, embed_status = NULL, embed_failures = 0, embed_error = NULL
         WHERE slug = ?1",
        params![
            slug,
            patch.title,
            write.aliases,
            write.tags,
            write.entities,
            write.related,
            write.summary,
            write.article,
            history,
            write.search,
            write.history,
            write.at
        ],
    )
    .map_err(query)
}

pub(crate) fn rebase_merged(
    conn: &Connection,
    patch: &mut WikiPatch,
    base: &MergeBase,
) -> Result<(), WikiStoreError> {
    let Some(row) = load(conn, patch.slug.as_ref())? else {
        return Ok(());
    };
    patch.summary = stable_summary(&row.current_state, &patch.summary);
    patch.article = patch
        .article
        .take()
        .map(|article| rebase(&base.article, &article, &row.article));
    Ok(())
}

fn load(conn: &Connection, slug: &str) -> Result<Option<Existing>, WikiStoreError> {
    conn.query_row(
        "SELECT title, aliases, tags, entities, current_state, article, related
           FROM ros_wiki_topics WHERE slug = ?1",
        params![slug],
        |row| {
            Ok(Existing {
                title: row.get(0)?,
                aliases: row.get(1)?,
                tags: row.get(2)?,
                entities: row.get(3)?,
                current_state: row.get(4)?,
                article: row.get(5)?,
                related: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(query)
}

fn canonical(
    conn: &Connection,
    patch: &WikiPatch,
    page: Option<&WikiSlug>,
) -> Result<String, WikiStoreError> {
    if let Some(page) = page.filter(|page| **page == patch.slug) {
        return Ok(page.as_ref().to_owned());
    }
    let page = page.map(AsRef::as_ref);
    let mut candidates = vec![patch.slug.as_ref().to_owned()];
    candidates.extend(patch.aliases.iter().map(|alias| alias.as_ref().to_owned()));
    let mut found = BTreeSet::new();
    let mut blocked = false;
    for (index, candidate) in candidates.iter().enumerate() {
        for (slug, explicit) in matches(conn, candidate)? {
            if (index == 0 && explicit) || open(conn, &slug, page)? {
                found.insert(slug);
            } else {
                blocked = true;
            }
        }
    }
    if found.is_empty()
        && let Some(slug) = same_identity(conn, patch)?
    {
        if open(conn, &slug, page)? {
            found.insert(slug);
        } else {
            blocked = true;
        }
    }
    let mut found = found.into_iter();
    match (found.next(), found.next()) {
        (None, _) if patch.action == WikiAction::Create || blocked => {
            Ok(patch.slug.as_ref().to_owned())
        }
        (None, _) => Err(WikiStoreError::TopicMissing(patch.slug.as_ref().to_owned())),
        (Some(slug), None) => Ok(slug),
        (Some(_), Some(_)) => Err(WikiStoreError::SlugConflict(patch.slug.as_ref().to_owned())),
    }
}

fn same_identity(conn: &Connection, patch: &WikiPatch) -> Result<Option<String>, WikiStoreError> {
    let identity = topic_identity(patch.slug.as_ref(), &patch.title);
    let Some(probe) = identity.probe() else {
        return Ok(None);
    };
    let mut statement = conn
        .prepare("SELECT slug, title FROM ros_wiki_topics WHERE instr(slug, ?1) > 0")
        .map_err(query)?;
    let rows = statement
        .query_map(params![probe], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(query)?;
    let candidates = rows
        .collect::<Result<Vec<(String, String)>, _>>()
        .map_err(query)?;
    Ok(identity.closest(candidates))
}

fn open(conn: &Connection, slug: &str, page: Option<&str>) -> Result<bool, WikiStoreError> {
    if page == Some(slug) {
        return Ok(true);
    }
    conn.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM rh_topic_pages
                             WHERE page_slug = ?1 AND topic_id IS NOT NULL)",
        params![slug],
        |row| row.get(0),
    )
    .map_err(query)
}

fn matches(conn: &Connection, candidate: &str) -> Result<Vec<(String, bool)>, WikiStoreError> {
    let mut statement = conn
        .prepare(
            "SELECT slug, 1 FROM ros_wiki_topics WHERE slug = ?1
             UNION
             SELECT t.slug, 0 FROM ros_wiki_topics t, json_each(t.aliases) AS item
              WHERE item.value = ?1
             UNION
             SELECT to_slug, 1 FROM ros_wiki_redirects WHERE from_slug = ?1",
        )
        .map_err(query)?;
    let rows = statement
        .query_map(params![candidate], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(query)?;
    let mut slugs = Vec::new();
    for row in rows {
        slugs.push(row.map_err(query)?);
    }
    Ok(slugs)
}

fn redirects(
    conn: &Connection,
    slug: &str,
    aliases: &[String],
    at: &str,
) -> Result<(), WikiStoreError> {
    for alias in aliases {
        let occupied: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ros_wiki_topics WHERE slug = ?1)",
                params![alias],
                |row| row.get(0),
            )
            .map_err(query)?;
        if occupied != 0 {
            return Err(WikiStoreError::SlugConflict(alias.clone()));
        }
        let written = conn
            .execute(
                "INSERT INTO ros_wiki_redirects (from_slug, to_slug, created_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(from_slug) DO UPDATE SET to_slug = excluded.to_slug
                 WHERE ros_wiki_redirects.to_slug = excluded.to_slug",
                params![alias, slug, at],
            )
            .map_err(query)?;
        if written != 1 {
            return Err(WikiStoreError::SlugConflict(alias.clone()));
        }
    }
    Ok(())
}

fn provenance(
    conn: &Connection,
    slug: &str,
    summary: &WikiSummary,
    note: Option<&str>,
    at: &str,
) -> Result<(), WikiStoreError> {
    let conversation = summary.conversation_id.map(|id| id.to_string());
    conn.execute(
        "INSERT INTO ros_wiki_provenance
            (topic_slug, source_kind, source_id, conversation_id, created_at)
         VALUES (?1, 'summary', ?2, ?3, ?4) ON CONFLICT DO NOTHING",
        params![slug, summary.id.to_string(), conversation, at],
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
         VALUES (?1, ?2, 'summary', ?3, ?4) ON CONFLICT DO NOTHING",
        params![slug, summary.id.to_string(), note, at],
    )
    .map_err(query)?;
    Ok(())
}

fn alias_list(patch: &WikiPatch, slug: &str) -> Vec<String> {
    let mut aliases: Vec<String> = patch
        .aliases
        .iter()
        .map(|alias| alias.as_ref().to_owned())
        .chain((slug != patch.slug.as_ref()).then(|| patch.slug.as_ref().to_owned()))
        .filter(|alias| alias != slug)
        .collect();
    aliases.sort();
    aliases.dedup();
    aliases
}

fn stored_article(article: Option<&str>, existing: Option<&Existing>) -> String {
    match article {
        Some(text) => text.to_owned(),
        None => match existing {
            Some(row) => row.article.clone(),
            None => String::new(),
        },
    }
}

fn stored_related(
    related: Option<&Vec<kernel::wiki::page::WikiSlug>>,
    existing: Option<&Existing>,
) -> Result<String, WikiStoreError> {
    match related {
        Some(slugs) => {
            let values = slugs
                .iter()
                .map(|slug| slug.as_ref().to_owned())
                .collect::<Vec<_>>();
            json(&values)
        }
        None => match existing {
            Some(row) => Ok(row.related.clone()),
            None => Ok("[]".to_owned()),
        },
    }
}

fn search_text(title: &str, state: &str, lists: [&[String]; 3], article: Option<&str>) -> String {
    let [aliases, tags, entities] = lists.map(|list| list.join(" "));
    let prefix = format!("{title}\n{state}\n{aliases}\n{tags}\n{entities}");
    match article {
        Some(text) => format!("{prefix}\n{text}"),
        None => prefix,
    }
}

fn note(
    conn: &Connection,
    summary: &WikiSummary,
    patch: &WikiPatch,
    slug: &str,
    row: &Existing,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    let proposed = patch.slug.as_ref().to_owned();
    let mut aliases = list(&row.aliases)?;
    if !aliases.contains(&proposed) {
        aliases.push(proposed.clone());
    }
    let search = search_text(
        &row.title,
        &row.current_state,
        [aliases.as_slice(), &list(&row.tags)?, &list(&row.entities)?],
        Some(&row.article),
    );
    let at = time::stamp(now);
    let history = crate::wiki_history::encode(&crate::wiki_history::with_entry(
        crate::wiki_history::load(conn, slug)?,
        Some(&patch.note(now.utc_date())),
    ))?;
    conn.execute(
        "UPDATE ros_wiki_topics SET aliases = ?2, search_text = ?3, history = ?4,
            history_count = history_count + 1, last_verified_at = ?5, updated_at = ?5,
            embedding = NULL, embed_status = NULL, embed_failures = 0, embed_error = NULL
         WHERE slug = ?1",
        params![slug, json(&aliases)?, search, history, at],
    )
    .map_err(query)?;
    redirects(conn, slug, std::slice::from_ref(&proposed), &at)?;
    provenance(
        conn,
        slug,
        summary,
        Some(&patch.note(now.utc_date()).body),
        &at,
    )
}

fn list(text: &str) -> Result<Vec<String>, WikiStoreError> {
    serde_json::from_str(text).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn json(values: &[String]) -> Result<String, WikiStoreError> {
    serde_json::to_string(values).map_err(|source| WikiStoreError::Query(Box::new(source)))
}

fn query(source: rusqlite::Error) -> WikiStoreError {
    WikiStoreError::Query(Box::new(source))
}
