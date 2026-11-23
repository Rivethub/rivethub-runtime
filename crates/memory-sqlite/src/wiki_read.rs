use std::collections::BTreeMap;
use std::collections::BTreeSet;

use kernel::memory::MemoryStoreError;
use kernel::memory::Provenance;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiGapsResponse;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiPageResponse;
use kernel::wiki::WikiRedLink;
use kernel::wiki::WikiSource;
use kernel::wiki::WikiTopic;
use kernel::wiki::WikiTopicList;
use kernel::wiki::page::WikiHistoryEntry;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::WikiSourceKind;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::search::fts_or;
use crate::taxonomy::slug;
use crate::time;

const EXCERPT_CHARS: usize = 240;

struct Topic {
    slug: String,
    title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    current_state: String,
    updated_at: String,
    owner: Option<String>,
}

impl SqliteStore {
    pub(crate) async fn read_wiki_topics(
        &self,
        filter: WikiFilter,
    ) -> Result<WikiTopicList, MemoryStoreError> {
        self.with_conn(move |conn| list_topics(conn, &filter))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn read_wiki_page(
        &self,
        slug: &WikiSlug,
    ) -> Result<WikiLookup, MemoryStoreError> {
        let slug = slug.as_ref().to_owned();
        self.with_conn(move |conn| page(conn, &slug))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn read_wiki_imported_authors(
        &self,
        slugs: Vec<String>,
    ) -> Result<BTreeMap<String, Vec<Provenance>>, MemoryStoreError> {
        self.with_conn(move |conn| crate::wiki_citations::imported_authors(conn, &slugs))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn read_wiki_gaps(
        &self,
        limit: u32,
    ) -> Result<WikiGapsResponse, MemoryStoreError> {
        self.with_conn(move |conn| gaps(conn, limit))
            .await
            .map_err(MemoryStoreError::from)
    }
}

fn list_topics(conn: &Connection, filter: &WikiFilter) -> Result<WikiTopicList, SqliteError> {
    let (where_sql, params) = topic_filter(filter);
    let total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM ros_wiki_topics WHERE {where_sql}"),
            params_from_iter(params.iter()),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("wiki topic count"))?;
    let mut listed = params;
    listed.push(Value::Integer(i64::from(filter.limit)));
    listed.push(Value::Integer(i64::from(filter.offset)));
    let mut statement = conn
        .prepare(&format!(
            "SELECT slug, title, aliases, tags, entities, current_state, updated_at, \
                    (SELECT topic_id FROM rh_topic_pages WHERE page_slug = ros_wiki_topics.slug) \
               FROM ros_wiki_topics WHERE {where_sql} \
              ORDER BY updated_at DESC, slug ASC LIMIT ? OFFSET ?"
        ))
        .map_err(SqliteError::query("wiki topics"))?;
    let rows = statement
        .query_map(params_from_iter(listed.iter()), read_topic)
        .map_err(SqliteError::query("wiki topics"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki topics"))?;
    let topics = rows
        .into_iter()
        .map(topic_wire)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WikiTopicList {
        topics,
        total: u64::try_from(total).map_err(SqliteError::Count)?,
    })
}

fn topic_filter(filter: &WikiFilter) -> (String, Vec<Value>) {
    let mut sql = String::from("1 = 1");
    let mut params = Vec::new();
    if let Some(text) = &filter.query {
        sql.push_str(" AND (instr(lower(title), lower(?)) > 0 OR instr(lower(slug), lower(?)) > 0");
        params.push(Value::Text(text.clone()));
        params.push(Value::Text(text.clone()));
        if let Some(pattern) = fts_or(text) {
            sql.push_str(
                " OR slug IN (SELECT slug FROM ros_wiki_topics_fts WHERE ros_wiki_topics_fts MATCH ?)",
            );
            params.push(Value::Text(pattern));
        }
        sql.push(')');
    }
    if let Some(tag) = &filter.tag {
        sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(tags) WHERE json_each.value = ?)");
        params.push(Value::Text(tag.clone()));
    }
    (sql, params)
}

fn page(conn: &Connection, slug: &str) -> Result<WikiLookup, SqliteError> {
    let found = conn
        .query_row(
            "SELECT slug, title, aliases, tags, entities, current_state, article, related, \
                    git_sha, last_verified_at, updated_at, history, \
                    (SELECT topic_id FROM rh_topic_pages WHERE page_slug = ros_wiki_topics.slug), \
                    (SELECT sections FROM rh_topic_pages WHERE page_slug = ros_wiki_topics.slug) \
               FROM ros_wiki_topics WHERE slug = ?1",
            params_from_iter([Value::Text(slug.to_owned())]),
            |row| {
                Ok(PageRow {
                    slug: row.get(0)?,
                    title: row.get(1)?,
                    aliases: row.get(2)?,
                    tags: row.get(3)?,
                    entities: row.get(4)?,
                    state: row.get(5)?,
                    article: row.get(6)?,
                    related: row.get(7)?,
                    git_sha: row.get(8)?,
                    verified: row.get(9)?,
                    updated: row.get(10)?,
                    history: row.get(11)?,
                    topic_id: row.get(12)?,
                    sections: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(SqliteError::query("wiki page"))?;
    let Some(row) = found else {
        return redirect(conn, slug);
    };
    assemble(conn, row)
}

struct PageRow {
    slug: String,
    title: String,
    aliases: String,
    tags: String,
    entities: String,
    state: String,
    article: String,
    related: String,
    git_sha: Option<String>,
    verified: Option<String>,
    updated: String,
    history: String,
    topic_id: Option<String>,
    sections: Option<String>,
}

fn assemble(conn: &Connection, row: PageRow) -> Result<WikiLookup, SqliteError> {
    let article_text = if row.article.is_empty() {
        None
    } else {
        Some(row.article)
    };
    let sections = sections_markdown(row.sections.as_deref())?;
    let markdown = page_markdown(
        conn,
        &row.title,
        &row.state,
        article_text.as_deref(),
        &sections,
        row.topic_id.as_deref(),
    )?;
    Ok(WikiLookup::Page(Box::new(WikiPageResponse {
        slug: row.slug.clone(),
        title: row.title,
        aliases: json_list(&row.aliases, "aliases")?,
        tags: json_list(&row.tags, "tags")?,
        entities: json_list(&row.entities, "entities")?,
        current_state: row.state,
        article: article_text,
        see_also: None,
        history: history_entries(&row.history)?,
        citations: crate::wiki_citations::citations(conn, &row.slug)?,
        markdown,
        sources: sources(conn, &row.slug)?,
        git_sha: row.git_sha,
        last_verified: row
            .verified
            .as_deref()
            .map(|text| time::stored(text, "last_verified_at"))
            .transpose()?,
        updated_at: time::stored(&row.updated, "updated_at")?,
        related: Some(json_list(&row.related, "related")?),
        topic_id: row.topic_id,
    })))
}

fn page_markdown(
    conn: &Connection,
    title: &str,
    state: &str,
    article: Option<&str>,
    sections: &str,
    topic: Option<&str>,
) -> Result<String, SqliteError> {
    let mut markdown = match article {
        None => format!("# {title}\n\n{state}\n{sections}"),
        Some(body) => format!("# {title}\n\n{state}\n\n{body}\n{sections}"),
    };
    if let Some(topic) = topic {
        let history = crate::entity_builder::history_markdown(conn, topic)?;
        if !history.is_empty() {
            markdown.push('\n');
            markdown.push_str(&history);
        }
    }
    Ok(markdown)
}

fn redirect(conn: &Connection, slug: &str) -> Result<WikiLookup, SqliteError> {
    let target: Option<String> = conn
        .query_row(
            "SELECT r.to_slug FROM ros_wiki_redirects r \
               JOIN ros_wiki_topics t ON t.slug = r.to_slug \
              WHERE r.from_slug = ?1",
            params_from_iter([Value::Text(slug.to_owned())]),
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("wiki redirect"))?;
    Ok(match target {
        Some(slug) => WikiLookup::Redirect(slug),
        None => WikiLookup::Missing,
    })
}

fn gaps(conn: &Connection, limit: u32) -> Result<WikiGapsResponse, SqliteError> {
    let limit = i64::from(limit.clamp(1, kernel::wiki::WIKI_LIMIT_MAX));
    let topics = all_topics(conn)?;
    let redirects = redirect_slugs(conn)?;
    let mut grouped: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for topic in &topics {
        for entity in &topic.entities {
            if known(entity, &topics, &redirects) {
                continue;
            }
            grouped
                .entry(entity.clone())
                .or_default()
                .insert(topic.slug.clone());
        }
    }
    let mut red_links: Vec<WikiRedLink> = grouped
        .into_iter()
        .map(|(entity, slugs)| WikiRedLink {
            entity,
            referenced_by: slugs.into_iter().collect(),
        })
        .collect();
    red_links.sort_by(|left, right| {
        right
            .referenced_by
            .len()
            .cmp(&left.referenced_by.len())
            .then(left.entity.cmp(&right.entity))
    });
    let capped = usize::try_from(limit).map_err(SqliteError::Count)?;
    red_links.truncate(capped);
    let mut statement = conn
        .prepare(
            "SELECT slug, title, aliases, tags, entities, current_state, updated_at, \
                    (SELECT topic_id FROM rh_topic_pages WHERE page_slug = ros_wiki_topics.slug) \
               FROM ros_wiki_topics \
              ORDER BY coalesce(last_verified_at, updated_at), slug LIMIT ?1",
        )
        .map_err(SqliteError::query("wiki stale"))?;
    let stalest = statement
        .query_map(params_from_iter([Value::Integer(limit)]), read_topic)
        .map_err(SqliteError::query("wiki stale"))?
        .collect::<Result<Vec<Topic>, _>>()
        .map_err(SqliteError::query("wiki stale"))?
        .into_iter()
        .map(topic_wire)
        .collect::<Result<Vec<WikiTopic>, _>>()?;
    Ok(WikiGapsResponse { red_links, stalest })
}

fn known(entity: &str, topics: &[Topic], redirects: &[String]) -> bool {
    if redirects
        .iter()
        .any(|item| item.eq_ignore_ascii_case(entity))
    {
        return true;
    }
    let tail = match entity.split_once(':') {
        Some((_, tail)) => tail,
        None => entity,
    };
    let tail_slug = slug(tail);
    let prefixed = format!("{tail_slug}-");
    topics.iter().any(|topic| {
        topic.slug.eq_ignore_ascii_case(entity)
            || topic.title.eq_ignore_ascii_case(entity)
            || topic
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(entity))
            || topic.slug.eq_ignore_ascii_case(tail)
            || topic.slug.eq_ignore_ascii_case(&tail_slug)
            || (!tail_slug.is_empty() && topic.slug.to_lowercase().starts_with(&prefixed))
    })
}

fn all_topics(conn: &Connection) -> Result<Vec<Topic>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT slug, title, aliases, tags, entities, current_state, updated_at, \
                    (SELECT topic_id FROM rh_topic_pages WHERE page_slug = ros_wiki_topics.slug) \
               FROM ros_wiki_topics",
        )
        .map_err(SqliteError::query("wiki gaps"))?;
    statement
        .query_map([], read_topic)
        .map_err(SqliteError::query("wiki gaps"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki gaps"))
}

fn redirect_slugs(conn: &Connection) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT from_slug FROM ros_wiki_redirects")
        .map_err(SqliteError::query("wiki redirects"))?;
    statement
        .query_map([], |row| row.get(0))
        .map_err(SqliteError::query("wiki redirects"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki redirects"))
}

fn sources(conn: &Connection, slug: &str) -> Result<Vec<WikiSource>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT source_kind, source_id, conversation_id FROM ros_wiki_provenance \
              WHERE topic_slug = ?1 ORDER BY source_kind, source_id",
        )
        .map_err(SqliteError::query("wiki sources"))?;
    let rows = statement
        .query_map(params_from_iter([Value::Text(slug.to_owned())]), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(SqliteError::query("wiki sources"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("wiki sources"))?;
    rows.into_iter()
        .map(|(kind, id, conversation_id)| {
            Ok(WikiSource {
                kind: kind
                    .parse::<WikiSourceKind>()
                    .map_err(|source| SqliteError::Query {
                        operation: "wiki source kind",
                        source: rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(source),
                        ),
                    })?,
                ids: vec![id],
                conversation_id,
                span: None,
            })
        })
        .collect()
}

fn read_topic(row: &rusqlite::Row<'_>) -> rusqlite::Result<Topic> {
    Ok(Topic {
        slug: row.get(0)?,
        title: row.get(1)?,
        aliases: string_list(&row.get::<_, String>(2)?)?,
        tags: string_list(&row.get::<_, String>(3)?)?,
        entities: string_list(&row.get::<_, String>(4)?)?,
        current_state: row.get(5)?,
        updated_at: row.get(6)?,
        owner: row.get(7)?,
    })
}

fn string_list(text: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(text).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(source))
    })
}

fn topic_wire(topic: Topic) -> Result<WikiTopic, SqliteError> {
    Ok(WikiTopic {
        slug: topic.slug,
        title: topic.title,
        tags: topic.tags,
        entities: topic.entities,
        updated_at: time::stored(&topic.updated_at, "updated_at")?,
        excerpt: topic.current_state.chars().take(EXCERPT_CHARS).collect(),
        topic_id: topic.owner,
    })
}

fn sections_markdown(sections: Option<&str>) -> Result<String, SqliteError> {
    crate::topic_pages::rendered(sections).map_err(|source| SqliteError::Query {
        operation: "sections",
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}

fn history_entries(text: &str) -> Result<Vec<WikiHistoryEntry>, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::StoredJson {
        column: "history",
        source,
    })
}

fn json_list(text: &str, column: &'static str) -> Result<Vec<String>, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::Query {
        operation: column,
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}
