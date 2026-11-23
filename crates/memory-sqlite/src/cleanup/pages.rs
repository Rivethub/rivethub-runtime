use std::collections::BTreeMap;
use std::str::FromStr;

use kernel::cleanup::EntitySection;
use kernel::cleanup::KnowledgePage;
use kernel::cleanup::Project;
use kernel::cleanup::SnapshotRoot;
use kernel::cleanup::SnapshotTopic;
use kernel::memory::TagState;
use kernel::topics::Root;
use kernel::topics::RootKind;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use rusqlite::Connection;
use rusqlite::params;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::SqliteError;
use crate::time;

const LEAD_CHARS: usize = 320;
const LEAD_LINES: usize = 3;

pub(super) fn topics(conn: &Connection) -> Result<Vec<SnapshotTopic>, SqliteError> {
    let aliases = aliases(conn)?;
    let mut statement = conn
        .prepare(
            "SELECT id, kind, name, parent_id, directory_kind, directory_node, directory_path,
                    created_at, updated_at
               FROM rh_topics
              ORDER BY rh_instant(created_at), id",
        )
        .map_err(failed("cleanup topics"))?;
    let rows = statement
        .query_map([], topic_text)
        .map_err(failed("cleanup topics"))?;
    let mut topics = Vec::new();
    for row in rows {
        topics.push(topic_from(
            row.map_err(failed("cleanup topics"))?,
            &aliases,
        )?);
    }
    Ok(topics)
}

pub(super) fn projects(conn: &Connection) -> Result<(Vec<Project>, Vec<String>), SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT value, aliases, state FROM ros_tag_taxonomy
              WHERE key = 'project' ORDER BY created_at, value",
        )
        .map_err(failed("cleanup projects"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(failed("cleanup projects"))?;
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for row in rows {
        let (name, aliases, state) = row.map_err(failed("cleanup projects"))?;
        let state = TagState::from_str(&state).map_err(|_| stored("state", state))?;
        match state {
            TagState::Accepted => accepted.push(Project {
                name,
                aliases: json_list(&aliases, "aliases")?,
            }),
            TagState::Rejected => refused.push(name),
            TagState::Suggested => {}
        }
    }
    refused.extend(tombstones(conn)?);
    Ok((accepted, refused))
}

pub(super) fn knowledge(conn: &Connection) -> Result<Vec<KnowledgePage>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT slug, title, current_state, entities FROM ros_wiki_topics w
              WHERE slug NOT IN ('people', 'hosts', 'tools')
                AND NOT EXISTS (
                    SELECT 1 FROM rh_topic_pages p
                     WHERE p.page_slug = w.slug AND p.topic_id IS NOT NULL
                )
              ORDER BY slug",
        )
        .map_err(failed("cleanup pages"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(failed("cleanup pages"))?;
    let mut pages = Vec::new();
    for row in rows {
        let (slug, title, state, entities) = row.map_err(failed("cleanup pages"))?;
        pages.push(KnowledgePage {
            slug: slug.clone(),
            title,
            lead: lead(&state),
            entities: json_list(&entities, "entities")?,
            cited: cited(conn, &slug)?,
        });
    }
    Ok(pages)
}

pub(super) fn sections(conn: &Connection) -> Result<Vec<EntitySection>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT page_slug, sections FROM rh_topic_pages
              WHERE page_slug IN ('people', 'hosts', 'tools')
              ORDER BY page_slug",
        )
        .map_err(failed("cleanup sections"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(failed("cleanup sections"))?;
    let mut sections = Vec::new();
    for row in rows {
        let (page, body) = row.map_err(failed("cleanup sections"))?;
        sections.extend(entity_sections(&page, &body)?);
    }
    Ok(sections)
}

fn aliases(conn: &Connection) -> Result<BTreeMap<String, Vec<String>>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT topic_id, alias FROM rh_topic_aliases ORDER BY alias")
        .map_err(failed("cleanup aliases"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(failed("cleanup aliases"))?;
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        let (topic, alias) = row.map_err(failed("cleanup aliases"))?;
        aliases.entry(topic).or_default().push(alias);
    }
    Ok(aliases)
}

struct TopicText {
    id: String,
    kind: String,
    name: String,
    parent: Option<String>,
    directory_kind: Option<String>,
    directory_node: Option<String>,
    directory_path: Option<String>,
    created_at: String,
    updated_at: String,
}

fn topic_text(row: &rusqlite::Row<'_>) -> rusqlite::Result<TopicText> {
    Ok(TopicText {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        parent: row.get(3)?,
        directory_kind: row.get(4)?,
        directory_node: row.get(5)?,
        directory_path: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn topic_from(
    row: TopicText,
    aliases: &BTreeMap<String, Vec<String>>,
) -> Result<SnapshotTopic, SqliteError> {
    Ok(SnapshotTopic {
        id: topic_id(&row.id)?,
        kind: TopicKind::from_str(&row.kind).map_err(|_| stored("kind", row.kind))?,
        name: row.name,
        parent: row.parent.as_deref().map(topic_id).transpose()?,
        aliases: aliases.get(&row.id).cloned().unwrap_or_default(),
        directory: directory(row.directory_kind, row.directory_node, row.directory_path)?,
        created_at: time::stored(&row.created_at, "created_at")?,
        updated_at: time::stored(&row.updated_at, "updated_at")?,
    })
}

fn directory(
    kind: Option<String>,
    node: Option<String>,
    path: Option<String>,
) -> Result<Option<SnapshotRoot>, SqliteError> {
    let Some(kind) = kind else {
        return Ok(None);
    };
    let parsed = RootKind::from_str(&kind).map_err(|_| stored("directory_kind", kind))?;
    let path = path.ok_or_else(|| stored("directory_path", String::new()))?;
    let root = Root::from_parts(parsed, node.clone(), path)
        .ok_or_else(|| stored("directory_node", node.unwrap_or_default()))?;
    Ok(Some(SnapshotRoot::of(&root)))
}

fn tombstones(conn: &Connection) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT slug FROM rh_topic_tombstones WHERE kind = 'work' ORDER BY slug")
        .map_err(failed("cleanup tombstones"))?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(failed("cleanup tombstones"))?;
    let mut slugs = Vec::new();
    for slug in rows {
        slugs.push(slug.map_err(failed("cleanup tombstones"))?);
    }
    Ok(slugs)
}

fn cited(conn: &Connection, slug: &str) -> Result<Vec<Uuid>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT conversation_id FROM (
                 SELECT s.conversation_id AS conversation_id
                   FROM ros_wiki_citations c
                   JOIN ros_summaries s ON s.id = c.summary_id
                  WHERE c.topic_slug = ?1 AND s.conversation_id IS NOT NULL
                 UNION
                 SELECT p.conversation_id
                   FROM ros_wiki_provenance p
                  WHERE p.topic_slug = ?1 AND p.conversation_id IS NOT NULL
             ) ORDER BY conversation_id",
        )
        .map_err(failed("cleanup citations"))?;
    let rows = statement
        .query_map(params![slug], |row| row.get::<_, String>(0))
        .map_err(failed("cleanup citations"))?;
    let mut cited = Vec::new();
    for row in rows {
        let text = row.map_err(failed("cleanup citations"))?;
        cited.push(Uuid::parse_str(&text).map_err(|_| stored("conversation_id", text))?);
    }
    Ok(cited)
}

#[derive(Deserialize)]
struct SectionBody {
    topic: Option<TopicId>,
    heading: String,
    body: String,
}

fn entity_sections(page: &str, body: &str) -> Result<Vec<EntitySection>, SqliteError> {
    let stored: Vec<SectionBody> =
        serde_json::from_str(body).map_err(|source| SqliteError::StoredJson {
            column: "sections",
            source,
        })?;
    let mut sections = Vec::new();
    for section in stored {
        let Some(topic) = section.topic else {
            continue;
        };
        sections.push(EntitySection {
            page: page.to_owned(),
            heading: section.heading,
            topic,
            lead: lead(&section.body),
        });
    }
    Ok(sections)
}

fn json_list(text: &str, column: &'static str) -> Result<Vec<String>, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::StoredJson { column, source })
}

fn topic_id(text: &str) -> Result<TopicId, SqliteError> {
    TopicId::from_str(text).map_err(|_| stored("topic_id", text.to_owned()))
}

fn stored(column: &'static str, value: String) -> SqliteError {
    SqliteError::StoredValue { column, value }
}

fn failed(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> SqliteError {
    SqliteError::query(operation)
}

fn lead(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>']).trim())
        .filter(|line| !line.is_empty())
        .take(LEAD_LINES)
        .collect();
    lines.join(" ").chars().take(LEAD_CHARS).collect()
}
