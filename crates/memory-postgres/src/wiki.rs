use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::WikiFilter;
use kernel::wiki::WikiGapsResponse;
use kernel::wiki::WikiLookup;
use kernel::wiki::WikiPageResponse;
use kernel::wiki::WikiRedLink;
use kernel::wiki::WikiSource;
use kernel::wiki::WikiTopic;
use kernel::wiki::WikiTopicList;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::archive::{
    ARCHIVE_DIRECTORY, archive_directory, legacy_archive_path, parse_archive_month,
};
use sqlx::Postgres;
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::Error;
use crate::PostgresMemory;
use crate::wiki_writer::archive::archive_parts;

#[derive(Debug, sqlx::FromRow)]
struct TopicRow {
    slug: String,
    title: String,
    tags: Vec<String>,
    entities: Vec<String>,
    current_state: String,
    updated_at: DateTime<Utc>,
    topic_id: Option<Uuid>,
}

fn topic_wire(row: TopicRow) -> WikiTopic {
    WikiTopic {
        slug: row.slug,
        title: row.title,
        tags: row.tags,
        entities: row.entities,
        updated_at: row.updated_at,
        excerpt: row.current_state.chars().take(240).collect(),
        topic_id: row.topic_id.map(|id| id.to_string()),
    }
}

fn topic_query(columns: &str, filter: &WikiFilter) -> QueryBuilder<Postgres> {
    let mut query = QueryBuilder::new(format!("SELECT {columns} FROM rh_wiki_topics WHERE true"));
    if let Some(text) = &filter.query {
        query.push(" AND (content_tsv @@ plainto_tsquery('english', ");
        query.push_bind(text);
        query.push(") OR strpos(lower(title), lower(");
        query.push_bind(text);
        query.push(")) > 0 OR strpos(lower(slug), lower(");
        query.push_bind(text);
        query.push(")) > 0)");
    }
    if let Some(tag) = &filter.tag {
        query.push(" AND ");
        query.push_bind(tag);
        query.push(" = ANY(tags)");
    }
    query
}

impl PostgresMemory {
    pub(crate) async fn read_wiki_topics(
        &self,
        filter: WikiFilter,
    ) -> Result<WikiTopicList, Error> {
        let mut transaction = self.pool.begin().await.map_err(Error::Connection)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(|source| Error::operation("wiki read snapshot", source))?;
        let (total,): (i64,) = topic_query("count(*)", &filter)
            .build_query_as()
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| Error::operation("wiki topic count", source))?;
        let rows: Vec<TopicRow> = topic_query(
            "slug, title, tags, entities, current_state, updated_at, topic_id",
            &filter,
        )
        .push(" ORDER BY updated_at DESC, slug LIMIT ")
        .push_bind(i64::from(filter.limit))
        .push(" OFFSET ")
        .push_bind(i64::from(filter.offset))
        .build_query_as()
        .fetch_all(&mut *transaction)
        .await
        .map_err(|source| Error::operation("wiki topics", source))?;
        transaction
            .commit()
            .await
            .map_err(|source| Error::operation("wiki read snapshot", source))?;
        Ok(WikiTopicList {
            topics: rows.into_iter().map(topic_wire).collect(),
            total: u64::try_from(total).map_err(|error| Error::protocol(error.to_string()))?,
        })
    }

    pub(crate) async fn read_wiki_page(&self, slug: &WikiSlug) -> Result<WikiLookup, Error> {
        let row = sqlx::query!(
            "SELECT slug, title, aliases, tags, entities, current_state, article, related,
                    git_sha, last_verified_at, updated_at, topic_id
             FROM rh_wiki_topics WHERE slug = $1",
            slug.as_ref()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki page", source))?;
        let Some(row) = row else {
            return self.wiki_redirect(slug).await;
        };
        let (citations, sources) =
            tokio::try_join!(self.read_citations(&row.slug), self.wiki_sources(&row.slug))?;
        let markdown = if let Some(directory) = &self.wiki_directory {
            let home = self.markdown_home(slug).await?;
            read_markdown(&directory.join(&home.directory), &home.local).await?
        } else if row.article.is_empty() {
            format!("# {}\n\n{}\n", row.title, row.current_state)
        } else {
            format!(
                "# {}\n\n{}\n\n{}\n",
                row.title, row.current_state, row.article
            )
        };
        Ok(WikiLookup::Page(Box::new(WikiPageResponse {
            slug: row.slug,
            title: row.title,
            aliases: row.aliases,
            tags: row.tags,
            entities: row.entities,
            current_state: row.current_state,
            article: (!row.article.is_empty()).then_some(row.article),
            see_also: None,
            history: Vec::new(),
            citations,
            markdown,
            sources,
            git_sha: row.git_sha,
            last_verified: row.last_verified_at,
            updated_at: row.updated_at,
            related: Some(row.related),
            topic_id: row.topic_id.map(|id| id.to_string()),
        })))
    }

    async fn markdown_home(&self, slug: &WikiSlug) -> Result<crate::wiki_scope::PageHome, Error> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|source| Error::operation("wiki page", source))?;
        crate::wiki_scope::page_home(&mut connection, slug.as_ref())
            .await
            .map_err(Error::WikiScope)
    }

    async fn wiki_redirect(&self, slug: &WikiSlug) -> Result<WikiLookup, Error> {
        let local = sqlx::query_scalar::<_, String>(
            "SELECT slug FROM rh_wiki_topics WHERE local_slug = $1
              ORDER BY (owner_user_id = rh_scope_principal()) DESC, slug LIMIT 1",
        )
        .bind(slug.as_ref())
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki redirect", source))?;
        if let Some(key) = local.filter(|key| key != slug.as_ref()) {
            return Ok(WikiLookup::Redirect(key));
        }
        let redirect = sqlx::query!(
            "SELECT r.to_slug FROM rh_wiki_redirects r
             JOIN rh_wiki_topics t ON t.slug = r.to_slug WHERE r.from_slug = $1",
            slug.as_ref()
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki redirect", source))?;
        Ok(match redirect {
            Some(row) => WikiLookup::Redirect(row.to_slug),
            None => WikiLookup::Missing,
        })
    }

    async fn wiki_sources(&self, slug: &str) -> Result<Vec<WikiSource>, Error> {
        let rows = sqlx::query!(
            "SELECT source_kind, source_id, conversation_id FROM rh_wiki_provenance
             WHERE topic_slug = $1 ORDER BY source_kind, source_id",
            slug
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki sources", source))?;
        rows.into_iter()
            .map(|row| {
                Ok(WikiSource {
                    kind: row
                        .source_kind
                        .parse::<kernel::wiki::page::WikiSourceKind>()
                        .map_err(|source| {
                            Error::operation(
                                "wiki source kind",
                                sqlx::Error::ColumnDecode {
                                    index: "source_kind".to_owned(),
                                    source: Box::new(source),
                                },
                            )
                        })?,
                    ids: vec![row.source_id.to_string()],
                    conversation_id: row.conversation_id.map(|id| id.to_string()),
                    span: None,
                })
            })
            .collect()
    }

    pub(crate) async fn read_wiki_gaps(&self, limit: u32) -> Result<WikiGapsResponse, Error> {
        let limit = i64::from(limit.clamp(1, kernel::wiki::WIKI_LIMIT_MAX));
        let red_links = sqlx::query!(
            "WITH known AS (
                 SELECT lower(slug) AS name FROM rh_wiki_topics
                 UNION SELECT lower(title) FROM rh_wiki_topics
                 UNION SELECT lower(alias) FROM rh_wiki_topics CROSS JOIN LATERAL unnest(aliases) alias
                 UNION SELECT lower(from_slug) FROM rh_wiki_redirects
             )
             SELECT entity AS \"entity!\", array_agg(DISTINCT t.slug ORDER BY t.slug) AS \"referenced_by!\"
             FROM rh_wiki_topics t CROSS JOIN LATERAL unnest(t.entities) entity
             WHERE NOT EXISTS (SELECT 1 FROM known WHERE known.name = lower(entity))
             GROUP BY entity ORDER BY count(DISTINCT t.slug) DESC, entity LIMIT $1",
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki red links", source))?;
        let stalest: Vec<TopicRow> = sqlx::query_as(
            "SELECT slug, title, tags, entities, current_state, updated_at, topic_id FROM rh_wiki_topics
             ORDER BY coalesce(last_verified_at, updated_at), slug LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("wiki stale topics", source))?;
        Ok(WikiGapsResponse {
            red_links: red_links
                .into_iter()
                .map(|row| WikiRedLink {
                    entity: row.entity,
                    referenced_by: row.referenced_by,
                })
                .collect(),
            stalest: stalest.into_iter().map(topic_wire).collect(),
        })
    }
}

impl PostgresMemory {
    pub(crate) async fn read_wiki_archive(
        &self,
        slug: &WikiSlug,
        month: Option<&str>,
    ) -> Result<Option<String>, Error> {
        let Some(directory) = self.wiki_directory.as_ref() else {
            return Ok(None);
        };
        let home = self.markdown_home(slug).await?;
        let root = tokio::fs::canonicalize(directory.join(&home.directory))
            .await
            .map_err(Error::WikiIo)?;
        let archives = root.join(ARCHIVE_DIRECTORY);
        let month = match month {
            Some(month) => match parse_archive_month(month) {
                Some(month) => Some(month),
                None => return Ok(None),
            },
            None => None,
        };
        let mut paths = Vec::new();
        if month.is_none() {
            paths.push(root.join(legacy_archive_path(&home.local)));
        }
        paths.extend(
            archive_parts(&root.join(archive_directory(&home.local)))
                .await
                .map_err(Error::WikiIo)?
                .into_iter()
                .filter(|(part, _)| month.as_ref().is_none_or(|month| part.month == *month))
                .map(|(_, path)| path),
        );
        let mut found: Option<String> = None;
        for path in paths {
            let path = match tokio::fs::canonicalize(path).await {
                Ok(path) => path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(Error::WikiIo(error)),
            };
            if !path.starts_with(&archives) {
                return Err(Error::WikiContainment);
            }
            let text = tokio::fs::read_to_string(path)
                .await
                .map_err(Error::WikiIo)?;
            found.get_or_insert_with(String::new).push_str(&text);
        }
        Ok(found)
    }
}

async fn read_markdown(directory: &std::path::Path, slug: &str) -> Result<String, Error> {
    let root = tokio::fs::canonicalize(directory)
        .await
        .map_err(Error::WikiIo)?;
    let topics = tokio::fs::canonicalize(root.join("topics"))
        .await
        .map_err(Error::WikiIo)?;
    if !topics.starts_with(&root) {
        return Err(Error::WikiContainment);
    }
    let path = tokio::fs::canonicalize(topics.join(format!("{slug}.md")))
        .await
        .map_err(Error::WikiIo)?;
    if !path.starts_with(&topics) {
        return Err(Error::WikiContainment);
    }
    tokio::fs::read_to_string(path).await.map_err(Error::WikiIo)
}
