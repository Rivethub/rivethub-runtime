//! Git page metadata is a cache. Store-backed reads take summary references from SQL.

use kernel::wiki::page::{
    CitationKind, PageCitation, PageSource, WikiPage, WikiPageError, WikiSourceKind,
    parse_wiki_page, serialize_wiki_page,
};
use kernel::wiki::{WikiCitation, WikiSource};

fn summary_source(kind: WikiSourceKind) -> bool {
    matches!(
        kind,
        WikiSourceKind::Summary | WikiSourceKind::Leaf | WikiSourceKind::ExtractWiki
    )
}

pub(super) fn clear(page: &mut WikiPage) -> bool {
    let had_citations = !page.citations.is_empty();
    let source_count = page.meta.sources.len();
    page.citations.clear();
    page.meta
        .sources
        .retain(|source| !summary_source(source.kind));
    had_citations || source_count != page.meta.sources.len()
}

pub(crate) fn project(
    markdown: String,
    citations: &[WikiCitation],
    sources: &[WikiSource],
) -> Result<String, WikiPageError> {
    // Legacy archives are ordinary Markdown. Some imported archives are whole wiki
    // pages and carry the same cached fields as current pages.
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(mut page) = parse_wiki_page(&markdown) else {
        return Ok(markdown);
    };
    let cleared = clear(&mut page);
    let citations = citations.iter().rev().take(40).rev();
    page.citations
        .extend(citations.map(|citation| PageCitation {
            summary_id: citation.summary_id.clone(),
            date: citation.date.map(|at| at.date_naive()),
            kind: match citation.kind.as_deref() {
                Some("leaf") => Some(CitationKind::Leaf),
                Some("branch") => Some(CitationKind::Branch),
                Some("root") => Some(CitationKind::Root),
                Some("condensed") => Some(CitationKind::Condensed),
                _ => None,
            },
            note: citation.note.clone(),
        }));
    page.meta.sources.extend(
        sources
            .iter()
            .filter(|source| summary_source(source.kind))
            .map(|source| PageSource {
                kind: source.kind,
                ids: source.ids.clone(),
                conversation_id: source.conversation_id.clone(),
                extra: wire::Extra::default(),
            }),
    );
    if cleared
        || !page.citations.is_empty()
        || sources.iter().any(|source| summary_source(source.kind))
    {
        serialize_wiki_page(&page)
    } else {
        Ok(markdown)
    }
}
