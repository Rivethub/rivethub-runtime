use std::collections::BTreeMap;
use std::str::FromStr;

use kernel::cleanup::EntitySection;
use kernel::cleanup::KnowledgePage;
use kernel::cleanup::SnapshotRoot;
use kernel::cleanup::SnapshotTopic;
use kernel::topics::Root;
use kernel::topics::RootKind;
use kernel::topics::TopicId;
use kernel::topics::TopicKind;
use kernel::wiki::page::WikiPage;
use kernel::wiki::page::WikiSlug;
use uuid::Uuid;

use super::rows::AliasRow;
use super::rows::PageRow;
use super::rows::TopicRow;
use crate::Error;
use crate::PostgresMemory;
use crate::wiki_writer::WikiRepository;

const LEAD_CHARS: usize = 320;
const LEAD_LINES: usize = 3;

impl PostgresMemory {
    pub(super) async fn snapshot_topics(&self) -> Result<Vec<SnapshotTopic>, Error> {
        let rows = sqlx::query_as::<_, TopicRow>(
            "WITH RECURSIVE legacy (id, kind) AS (
                 SELECT id, CASE WHEN slug IN ('people', 'hosts', 'tools') THEN 'type' ELSE 'work' END
                   FROM rh_topics WHERE parent_id IS NULL
                 UNION ALL
                 SELECT t.id, CASE WHEN l.kind = 'work' THEN 'work' ELSE 'entity' END
                   FROM rh_topics t JOIN legacy l ON t.parent_id = l.id
             )
             SELECT t.id, COALESCE(to_jsonb(t) ->> 'kind', l.kind) AS kind, t.name, t.parent_id,
                    t.directory_kind, t.directory_node, t.directory_path, t.created_at, t.updated_at
               FROM rh_topics t JOIN legacy l ON l.id = t.id
              ORDER BY t.created_at, t.id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup topics", source))?;
        let mut aliases: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
        for row in sqlx::query_as::<_, AliasRow>(
            "SELECT topic_id, alias FROM rh_topic_aliases ORDER BY alias",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup aliases", source))?
        {
            aliases.entry(row.topic_id).or_default().push(row.alias);
        }
        rows.into_iter()
            .map(|row| {
                let directory = directory(&row)?;
                Ok(SnapshotTopic {
                    id: TopicId::from_uuid(row.id),
                    kind: TopicKind::from_str(&row.kind)
                        .map_err(|source| Error::stored("kind", source))?,
                    name: row.name,
                    parent: row.parent_id.map(TopicId::from_uuid),
                    aliases: aliases.remove(&row.id).unwrap_or_default(),
                    directory,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                })
            })
            .collect()
    }

    pub(super) async fn snapshot_pages(&self) -> Result<Vec<KnowledgePage>, Error> {
        Ok(sqlx::query_as::<_, PageRow>(
            "SELECT w.slug, w.title, w.current_state, w.entities,
                    ARRAY(SELECT s.conversation_id
                            FROM rh_wiki_citations c
                            JOIN rh_summaries s ON s.id = c.summary_id
                           WHERE c.topic_slug = w.slug AND s.conversation_id IS NOT NULL
                          UNION
                          SELECT p.conversation_id
                            FROM rh_wiki_provenance p
                           WHERE p.topic_slug = w.slug AND p.conversation_id IS NOT NULL) AS cited
               FROM rh_wiki_topics w
              WHERE w.topic_id IS NULL AND w.slug NOT IN ('people', 'hosts', 'tools')
              ORDER BY w.slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup pages", source))?
        .into_iter()
        .map(|row| KnowledgePage {
            lead: lead(&row.current_state),
            slug: row.slug,
            title: row.title,
            entities: row.entities,
            cited: row.cited,
        })
        .collect())
    }

    pub(super) async fn snapshot_sections(&self) -> Result<Vec<EntitySection>, Error> {
        let Some(directory) = self.wiki_directory.clone() else {
            return Ok(Vec::new());
        };
        let repository = WikiRepository::for_store(directory);
        let slugs = sqlx::query_scalar::<_, String>(
            "SELECT slug FROM rh_wiki_topics WHERE slug IN ('people', 'hosts', 'tools') ORDER BY slug",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("cleanup category pages", source))?;
        let mut sections = Vec::new();
        for slug in slugs {
            let slug = WikiSlug::try_from(slug).map_err(|source| Error::stored("slug", source))?;
            let page = repository
                .read_unlocked(&slug)
                .await
                .map_err(|source| Error::stored("category page", source))?;
            if let Some(page) = page {
                sections.extend(entity_sections(&page)?);
            }
        }
        Ok(sections)
    }
}

fn entity_sections(page: &WikiPage) -> Result<Vec<EntitySection>, Error> {
    page.meta
        .sections
        .iter()
        .map(|section| {
            let body = page
                .extra_sections
                .iter()
                .find(|extra| extra.heading == section.heading)
                .map(|extra| extra.body.as_str());
            Ok(EntitySection {
                page: page.meta.slug.as_ref().to_owned(),
                heading: section.heading.clone(),
                topic: TopicId::from_uuid(Uuid::parse_str(&section.topic)?),
                lead: body.map(lead).unwrap_or_default(),
            })
        })
        .collect()
}

fn directory(row: &TopicRow) -> Result<Option<SnapshotRoot>, Error> {
    let Some(kind) = row.directory_kind.as_deref() else {
        return Ok(None);
    };
    let kind =
        RootKind::from_str(kind).map_err(|source| Error::stored("directory_kind", source))?;
    let path = row
        .directory_path
        .clone()
        .ok_or_else(|| Error::protocol("a topic directory without a path"))?;
    let root = Root::from_parts(kind, row.directory_node.clone(), path)
        .ok_or_else(|| Error::protocol("a topic directory node that does not match its kind"))?;
    Ok(Some(SnapshotRoot::of(&root)))
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
