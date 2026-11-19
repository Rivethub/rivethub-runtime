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

pub(crate) fn archive(markdown: String) -> Result<String, WikiPageError> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let projected = match parse_wiki_page(&markdown) {
        Ok(page) => project_page(markdown, page, &[], &[])?,
        Err(_) => markdown,
    };
    Ok(kernel::wiki::page::archive::without_summary_citations(
        &projected,
    ))
}

pub(crate) fn project(
    markdown: String,
    citations: &[WikiCitation],
    sources: &[WikiSource],
) -> Result<String, WikiPageError> {
    let page = parse_wiki_page(&markdown)?;
    project_page(markdown, page, citations, sources)
}

fn project_page(
    markdown: String,
    mut page: WikiPage,
    citations: &[WikiCitation],
    sources: &[WikiSource],
) -> Result<String, WikiPageError> {
    let cleared = clear(&mut page);
    let citations = citations.iter().rev().take(40).rev();
    page.citations
        .extend(citations.map(|citation| PageCitation {
            summary_id: citation.summary_id.clone(),
            date: citation.date.map(wire::time::TimestampFormat::utc_date),
            kind: citation.kind.as_deref().and_then(CitationKind::from_label),
            note: citation.note.clone(),
        }));
    page.meta.sources.extend(
        sources
            .iter()
            .filter(|source| summary_source(source.kind))
            .take(64)
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
