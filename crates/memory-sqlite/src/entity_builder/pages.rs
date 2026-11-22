use jiff::Timestamp;
use kernel::wiki::entity_pass::{
    EntityProse, EntityStoreError, EntityView, HistoryLine, PreparedHistory, PublishOutcome,
    entity_page, render_history, snapshot_of,
};
use kernel::wiki::page::{WikiPage, build_wiki_search_text};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};
use uuid::Uuid;

use super::{claims, error, fence, optional_uuid, query, rows, stamp, uuid};
use crate::summary_replace;

struct Head {
    name: String,
    slug: String,
    owner: Option<String>,
    type_name: Option<String>,
    type_slug: Option<String>,
}

impl Head {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            slug: row.get(1)?,
            owner: row.get(2)?,
            type_name: row.get(3)?,
            type_slug: row.get(4)?,
        })
    }
}

pub(super) fn view(conn: &Connection, id: Uuid) -> Result<EntityView, EntityStoreError> {
    let head = conn
        .query_row(
            "SELECT e.name, e.slug, e.owner_user_id, t.name, t.slug FROM rh_topics e
         LEFT JOIN rh_topics t ON t.id = e.parent_id WHERE e.id = ?1",
            [id.to_string()],
            Head::read,
        )
        .optional()
        .map_err(query)?
        .ok_or_else(|| EntityStoreError::SlugTaken(id.to_string()))?;
    let facts = claims::current(conn, id)?.facts;
    let history = history(conn, id)?;
    let snapshot = snapshot_of(&facts, &history);
    let aliases = rows(
        conn,
        "SELECT alias FROM rh_topic_aliases WHERE topic_id = ?1 AND kind = 'entity'
           AND alias <> ?2 ORDER BY alias_key",
        params![id.to_string(), head.name],
        |row| row.get(0),
    )?;
    Ok(EntityView {
        id,
        name: head.name,
        slug: head.slug,
        owner: head.owner,
        type_name: head.type_name.unwrap_or_default(),
        type_slug: head.type_slug.unwrap_or_default(),
        facts,
        history,
        aliases,
        snapshot,
    })
}

fn history(conn: &Connection, id: Uuid) -> Result<Vec<HistoryLine>, EntityStoreError> {
    rows(
        conn,
        "SELECT summary_id, conversation_id, session_on, sentence FROM rh_entity_history
         WHERE entity_id = ?1 ORDER BY session_on DESC, summary_id DESC",
        [id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get(3)?,
            ))
        },
    )?
    .into_iter()
    .map(|(summary, conversation, date, sentence)| {
        Ok(HistoryLine {
            summary_id: uuid(&summary)?,
            conversation_id: optional_uuid(conversation.as_deref())?,
            date: date.parse().map_err(error)?,
            sentence,
        })
    })
    .collect()
}

pub(crate) fn history_markdown(
    conn: &Connection,
    topic: &str,
) -> Result<String, crate::SqliteError> {
    let entries = uuid(topic)
        .and_then(|id| history(conn, id))
        .map_err(|error| crate::SqliteError::Invalid {
            message: format!("entity page history: {error}"),
        })?;
    if entries.is_empty() {
        Ok(String::new())
    } else {
        Ok(kernel::wiki::page::history_section(&render_history(
            &entries,
        )))
    }
}

pub(super) fn write_history(
    conn: &Connection,
    id: Uuid,
    line: &PreparedHistory,
    owner: &str,
    replace: bool,
) -> Result<(), EntityStoreError> {
    conn.execute(
        "INSERT INTO rh_entity_history
            (entity_id, summary_id, conversation_id, session_on, sentence, owner_user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (entity_id, summary_id) DO UPDATE SET sentence = excluded.sentence WHERE ?7",
        params![
            id.to_string(),
            line.summary_id.to_string(),
            line.conversation_id.map(|id| id.to_string()),
            line.session_on.to_string(),
            line.sentence,
            owner,
            replace
        ],
    )
    .map_err(query)?;
    Ok(())
}

pub(super) fn load_prose(
    conn: &Connection,
    view: &EntityView,
) -> Result<EntityProse, EntityStoreError> {
    conn.query_row(
        "SELECT w.current_state, w.article FROM ros_wiki_topics w
         LEFT JOIN rh_topic_pages p ON p.page_slug = w.slug
         WHERE p.topic_id = ?1 OR w.slug = ?2
         ORDER BY coalesce(p.topic_id = ?1, 0) DESC, rh_instant(w.updated_at) DESC LIMIT 1",
        params![view.id.to_string(), view.slug],
        |row| {
            Ok(EntityProse {
                summary: row.get(0)?,
                article: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(query)
    .map(Option::unwrap_or_default)
}

pub(super) fn publish(
    tx: &Transaction<'_>,
    prior: &EntityView,
    prose: &EntityProse,
    now: Timestamp,
) -> Result<PublishOutcome, EntityStoreError> {
    for line in &prior.history {
        summary_replace::shared(tx, line.summary_id).map_err(fence)?;
    }
    let fresh = view(tx, prior.id)?;
    if fresh.snapshot != prior.snapshot {
        return Ok(PublishOutcome::Stale(Box::new(fresh)));
    }
    store(tx, &fresh, prose, now)
}

pub(super) fn store(
    conn: &Connection,
    view: &EntityView,
    prose: &EntityProse,
    now: Timestamp,
) -> Result<PublishOutcome, EntityStoreError> {
    let page = entity_page(view, prose)?;
    write_page(conn, &page, now)?;
    bind(conn, view.id, &view.slug)?;
    cite(conn, view, &page, now)?;
    Ok(PublishOutcome::Stored {
        slug: view.slug.clone(),
        git_sha: String::new(),
    })
}

fn write_page(conn: &Connection, page: &WikiPage, now: Timestamp) -> Result<(), EntityStoreError> {
    let meta = &page.meta;
    let search = build_wiki_search_text(page);
    conn.execute(
        "INSERT INTO ros_wiki_topics (slug, title, aliases, tags, current_state, article,
                                     search_text, history_count, last_verified_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)
         ON CONFLICT (slug) DO UPDATE SET title = excluded.title, aliases = excluded.aliases,
            tags = excluded.tags, current_state = excluded.current_state, article = excluded.article,
            entities = excluded.entities, related = excluded.related, git_sha = NULL,
            search_text = excluded.search_text, history_count = excluded.history_count,
            last_verified_at = excluded.last_verified_at, updated_at = excluded.updated_at,
            embedding = NULL, embed_status = NULL, embed_failures = 0, embed_error = NULL",
        params![meta.slug.as_ref(), meta.title, serde_json::to_string(&meta.aliases).map_err(error)?,
            serde_json::to_string(&meta.tags).map_err(error)?, page.current_state, page.article, search,
            i64::try_from(page.history.len()).map_err(error)?, stamp(now)],
    ).map_err(query)?;
    Ok(())
}

pub(super) fn bind(conn: &Connection, entity: Uuid, slug: &str) -> Result<(), EntityStoreError> {
    let id = entity.to_string();
    conn.execute(
        "UPDATE rh_topic_pages SET topic_id = NULL WHERE topic_id = ?1 AND page_slug <> ?2",
        params![id, slug],
    )
    .map_err(query)?;
    conn.execute(
        "INSERT INTO rh_topic_pages (topic_id, page_slug)
         VALUES (?1, ?2) ON CONFLICT (page_slug) DO UPDATE SET topic_id = excluded.topic_id",
        params![id, slug],
    )
    .map_err(query)?;
    Ok(())
}

fn cite(
    conn: &Connection,
    view: &EntityView,
    page: &WikiPage,
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    let at = stamp(now);
    for citation in &page.citations {
        let conversation = view
            .history
            .iter()
            .find(|line| line.summary_id.to_string() == citation.summary_id)
            .and_then(|line| line.conversation_id)
            .map(|id| id.to_string());
        conn.execute(
            "INSERT INTO ros_wiki_citations (topic_slug, summary_id, kind, note, cited_at)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (topic_slug, summary_id) DO NOTHING",
            params![
                view.slug,
                citation.summary_id,
                citation.kind.map(|kind| kind.to_string()),
                citation.note,
                at
            ],
        )
        .map_err(query)?;
        conn.execute(
            "INSERT INTO ros_wiki_provenance (topic_slug, source_kind, source_id, conversation_id, created_at)
             VALUES (?1, 'summary', ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            params![view.slug, citation.summary_id, conversation, at],
        ).map_err(query)?;
    }
    Ok(())
}
