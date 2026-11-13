use crate::catalog::ToolInput as _;
use crate::{Error, text};
use kernel::memory::MemoryStore;
use kernel::wiki::WikiPageResponse;
use kernel::wiki::page::WikiSlug;
use kernel::wiki::page::{
    FullPageRefused, ReadSection, format_wiki_read, imported_banner, mark_imported_citations,
};
use kernel::wiki::{WikiFilter, WikiLookup};
use rmcp::model::{CallToolResult, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;

const FULL_PAGE_REFUSED: &str = "wiki page exceeds 24,000 characters; choose a section";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(!deny_unknown_fields)]
pub struct Search {
    #[schemars(description = "Topic to look for (name, alias, or content terms)")]
    query: String,
    #[schemars(description = "Max results (default 5)", range(min = 1, max = 20), transform = crate::catalog::optional_non_null)]
    limit: Option<u32>,
}

impl crate::catalog::ToolInput for Search {
    const NAME: &'static str = "wiki_search";
    const DESCRIPTION: &'static str = "Search the RivetHub memory wiki — curated topic pages distilled from conversation memory (\"what is currently true about X\"). Higher signal than memory_search for standing facts about projects, hosts, and services; use memory_search when you need what was actually said. Returns slugs — read the page (dated history + provenance) with wiki_read.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(!deny_unknown_fields)]
pub struct Read {
    #[schemars(description = "Topic slug, e.g. rh-task-engine", with = "String")]
    slug: WikiSlug,
    #[schemars(description = "Slice of an oversized page. Default: verbatim when small, encyclopedia view (Summary + Article + recent history) when large. full is refused while the page exceeds 24,000 characters.", transform = crate::catalog::optional_non_null)]
    section: Option<ReadSection>,
}

impl crate::catalog::ToolInput for Read {
    const NAME: &'static str = "wiki_read";
    const DESCRIPTION: &'static str = "Read one RivetHub wiki topic page: Wikipedia-style Summary (lead), Article body, See also crosslinks, dated History, and Citations (summary UUIDs usable with memory tools for drill-down). Use the slug from wiki_search. Small pages are returned verbatim. Pages larger than 24,000 characters (hub topics with thousands of YAML aliases) return a bounded encyclopedia view so MCP/capture truncation cannot hide Summary behind the alias dump. Pass section=summary|article|history|aliases|citations for a slice; section=full is refused on oversized pages.";

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

pub async fn search(store: &dyn MemoryStore, args: Search) -> Result<CallToolResult, Error> {
    let query = args.query.clone();
    let topics = store
        .wiki_topics(WikiFilter {
            query: Some(args.query),
            tag: None,
            limit: crate::memory::limit(Search::NAME, args.limit, 5, 20)?.get(),
            offset: 0,
        })
        .await?;
    if topics.topics.is_empty() {
        return Ok(text(format!("No wiki topics match \"{query}\".")));
    }
    let slugs = topics
        .topics
        .iter()
        .map(|topic| topic.slug.clone())
        .collect();
    let imported = store.wiki_imported_authors(slugs).await?;
    Ok(text(
        topics
            .topics
            .iter()
            .map(|topic| {
                let banner = imported
                    .get(&topic.slug)
                    .and_then(imported_banner)
                    .map(|banner| format!("{banner}\n"))
                    .unwrap_or_default();
                format!(
                    "## {} ({}) — verified {}\n{banner}{}",
                    topic.title,
                    topic.slug,
                    topic.updated_at.format("%Y-%m-%d"),
                    topic.excerpt
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    ))
}

pub(crate) async fn read(store: &dyn MemoryStore, args: Read) -> Result<CallToolResult, Error> {
    let page = lookup(store, args.slug).await?;
    let view = format_wiki_read(&page.markdown, &page.slug, args.section)
        .map_err(|FullPageRefused| Error::Refused(FULL_PAGE_REFUSED))?;
    let view = mark_imported_citations(&view, &page.citations);
    let banner = imported_banner(
        page.citations
            .iter()
            .filter_map(|citation| citation.imported.as_ref()),
    );
    Ok(text(match banner {
        Some(banner) => format!("{banner}\n\n{view}"),
        None => view,
    }))
}

async fn lookup(store: &dyn MemoryStore, mut slug: WikiSlug) -> Result<WikiPageResponse, Error> {
    let mut visited = HashSet::new();
    for _ in 0..16 {
        if !visited.insert(slug.as_ref().to_owned()) {
            return Err(Error::WikiRedirect);
        }
        match store.wiki_page(&slug).await? {
            WikiLookup::Page(page) => return Ok(*page),
            WikiLookup::Redirect(target) => slug = WikiSlug::try_from(target)?,
            WikiLookup::Missing => return Err(Error::WikiMissing(slug.into())),
        }
    }
    Err(Error::WikiRedirect)
}
