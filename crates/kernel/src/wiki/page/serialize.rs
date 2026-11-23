use super::WikiHistoryEntry;
use super::article::demote_h2_headings;
use super::frontmatter::WrittenFrontmatter;
use super::limits;
use super::yaml::to_block_yaml;
use super::{CitationKind, PageCitation, WikiPage, WikiPageError};
use values::time::LegacyStrftime as _;

pub(super) const CITATION_COLUMNS: &str =
    "| Date | Kind | Summary | Note |\n|------|------|---------|------|";

pub fn serialize_wiki_page(page: &WikiPage) -> Result<String, WikiPageError> {
    let related = related_links(page);
    let frontmatter = to_block_yaml(&WrittenFrontmatter {
        aliases: limits::first(&page.meta.aliases, limits::ALIASES),
        tags: limits::first(&page.meta.tags, limits::TAGS),
        sources: limits::last(&page.meta.sources, limits::SOURCES),
        ..WrittenFrontmatter::new(&page.meta, &related)
    })?;
    let links: Vec<String> = related.iter().map(|slug| format!("- [[{slug}]]")).collect();
    let mut parts = vec![
        format!(
            "## Summary\n\n{}\n",
            demote_h2_headings(&page.current_state).trim()
        ),
        open_section("Article", demote_h2_headings(&page.article).trim()),
        open_section("See also", &links.join("\n")),
        history_section(&page.history),
    ];
    if !page.citations.is_empty() {
        let rows: Vec<String> = limits::first(&page.citations, limits::CITATIONS)
            .iter()
            .map(citation_row)
            .collect();
        parts.push(format!(
            "## Citations\n\n{CITATION_COLUMNS}\n{}\n",
            rows.join("\n")
        ));
    }
    parts.push(extra_sections(page));
    let preamble = page
        .preamble
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("{text}\n\n"));
    Ok(format!(
        "---\n{frontmatter}---\n\n{}{}",
        preamble.unwrap_or_default(),
        parts.join("\n")
    ))
}

pub fn history_section(entries: &[WikiHistoryEntry]) -> String {
    let history: Vec<String> = limits::first(entries, limits::HISTORY)
        .iter()
        .map(|entry| {
            format!(
                "{}\n\n{}",
                history_heading(entry),
                demote_h2_headings(&entry.body).trim()
            )
        })
        .collect();
    format!("## History\n\n{}\n", history.join("\n\n").trim_end())
}

pub(super) fn history_heading(entry: &WikiHistoryEntry) -> String {
    let title = entry.title.trim();
    if title.is_empty() {
        format!("### {}", entry.date)
    } else {
        format!("### {} — {title}", entry.date)
    }
}

pub(super) fn citation_row(citation: &PageCitation) -> String {
    let date = citation.date.map(|date| date.legacy_strftime("%Y-%m-%d"));
    let note = citation
        .note
        .as_deref()
        .map(|note| note.replace('|', "\\|").replace('\n', " "));
    format!(
        "| {} | {} | `{}` | {} |",
        date.unwrap_or_default(),
        citation.kind.unwrap_or(CitationKind::Leaf),
        citation.summary_id,
        note.unwrap_or_default()
    )
}

fn open_section(heading: &str, content: &str) -> String {
    if content.is_empty() {
        String::new()
    } else {
        format!("## {heading}\n\n{content}\n\n")
    }
}

fn extra_sections(page: &WikiPage) -> String {
    if page.extra_sections.is_empty() {
        return String::new();
    }
    let sections: Vec<String> = page
        .extra_sections
        .iter()
        .map(|section| format!("## {}\n\n{}\n", section.heading, section.body))
        .collect();
    format!("\n{}", sections.join("\n"))
}

fn related_links(page: &WikiPage) -> Vec<String> {
    let mut related: Vec<String> = Vec::new();
    for slug in page.meta.related.iter().chain(&page.see_also) {
        if slug != page.meta.slug.as_ref() && !related.contains(slug) {
            related.push(slug.clone());
        }
    }
    related.truncate(limits::RELATED);
    related
}
