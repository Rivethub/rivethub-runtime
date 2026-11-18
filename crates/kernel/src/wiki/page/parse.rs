use jiff::civil::Date;
use strum::IntoEnumIterator as _;

use super::article::ArticleSection;
use super::citations::{iso_date, parse_citations, parse_see_also};
use super::frontmatter::StoredFrontmatter;
use super::lines::fence_aware;
use super::slug::normalize_slug;
use super::{
    PageCitation, PageDefect, WikiHistoryEntry, WikiMeta, WikiPage, WikiPageError, WikiSlug,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter, strum::Display)]
#[strum(serialize_all = "lowercase")]
enum Known {
    Summary,
    #[strum(serialize = "current state")]
    CurrentState,
    Article,
    #[strum(serialize = "see also")]
    SeeAlso,
    History,
    Citations,
}

impl Known {
    fn from_heading(heading: &str) -> Option<Self> {
        Self::iter().find(|known| known.to_string().eq_ignore_ascii_case(heading.trim()))
    }
}

#[derive(Debug, Default)]
struct Body {
    preamble: Option<String>,
    summary: Option<String>,
    current_state: Option<String>,
    article: Option<String>,
    see_also: Option<Vec<String>>,
    history: Option<Vec<WikiHistoryEntry>>,
    citations: Option<Vec<PageCitation>>,
    extra_sections: Vec<ArticleSection>,
}

impl Body {
    fn read(text: &str) -> Self {
        let mut body = Self::default();
        for (heading, section) in sections(text) {
            let section = section.trim();
            match heading.map(|heading| (Known::from_heading(heading), heading)) {
                None => body.preamble = Some(section.to_owned()).filter(|text| !text.is_empty()),
                Some((Some(Known::Summary), _)) => first(&mut body.summary, || section.to_owned()),
                Some((Some(Known::CurrentState), _)) => {
                    first(&mut body.current_state, || section.to_owned());
                }
                Some((Some(Known::Article), _)) => first(&mut body.article, || section.to_owned()),
                Some((Some(Known::SeeAlso), _)) => {
                    first(&mut body.see_also, || parse_see_also(section));
                }
                Some((Some(Known::History), _)) => {
                    first(&mut body.history, || parse_history(section));
                }
                Some((Some(Known::Citations), _)) => {
                    first(&mut body.citations, || parse_citations(section));
                }
                Some((None, heading)) => body.extra_sections.push(ArticleSection {
                    heading: heading.trim().to_owned(),
                    body: section.to_owned(),
                }),
            }
        }
        body
    }
}

fn first<T>(slot: &mut Option<T>, read: impl FnOnce() -> T) {
    if slot.is_none() {
        *slot = Some(read());
    }
}

pub fn parse_wiki_page(markdown: &str) -> Result<WikiPage, WikiPageError> {
    let markdown = markdown.replace("\r\n", "\n");
    let (frontmatter, body) = split_frontmatter(&markdown)
        .ok_or(WikiPageError::Invalid(PageDefect::MissingFrontmatter))?;
    let stored: Option<StoredFrontmatter> = serde_yaml_ng::from_str(frontmatter)?;
    let stored = stored.ok_or(WikiPageError::Invalid(PageDefect::FrontmatterNotMap))?;
    let title = stored
        .title
        .filter(|title| !title.trim().is_empty())
        .ok_or(WikiPageError::Invalid(PageDefect::TitleRequired))?;
    let slug = stored
        .slug
        .map(|slug| normalize_slug(&slug))
        .filter(|slug| !slug.is_empty())
        .ok_or(WikiPageError::Invalid(PageDefect::SlugRequired))?;
    let slug =
        WikiSlug::try_from(slug).map_err(|_| WikiPageError::Invalid(PageDefect::SlugRequired))?;
    let body = Body::read(body);
    let mut related: Vec<String> = Vec::new();
    for link in stored
        .related
        .iter()
        .map(|slug| normalize_slug(slug))
        .chain(body.see_also.into_iter().flatten())
    {
        if !link.is_empty() && !related.contains(&link) {
            related.push(link);
        }
    }
    Ok(WikiPage {
        meta: WikiMeta {
            title,
            slug,
            aliases: stored.aliases,
            tags: stored.tags,
            entities: stored.entities,
            related: related.clone(),
            last_verified: stored.last_verified,
            sources: stored.sources,
            sections: stored.sections,
            extra: stored.extra,
        },
        preamble: body.preamble,
        current_state: body.summary.or(body.current_state).unwrap_or_default(),
        article: body.article.unwrap_or_default(),
        history: body.history.unwrap_or_default(),
        citations: body.citations.unwrap_or_default(),
        see_also: related,
        extra_sections: body.extra_sections,
    })
}

fn split_frontmatter(markdown: &str) -> Option<(&str, &str)> {
    markdown.strip_prefix("---\n")?.split_once("\n---")
}

fn sections(body: &str) -> Vec<(Option<&str>, String)> {
    let mut sections: Vec<(Option<&str>, String)> = vec![(None, String::new())];
    for line in fence_aware(body) {
        let heading = line
            .text
            .strip_prefix("## ")
            .filter(|heading| !line.fenced && !heading.trim().is_empty());
        match (heading, sections.last_mut()) {
            (Some(heading), _) => sections.push((Some(heading), String::new())),
            (None, Some((_, text))) => {
                text.push_str(line.text);
                text.push('\n');
            }
            (None, None) => {}
        }
    }
    sections
}

fn parse_history(text: &str) -> Vec<WikiHistoryEntry> {
    let mut entries: Vec<WikiHistoryEntry> = Vec::new();
    for line in fence_aware(text) {
        match history_heading(line.text).filter(|_| !line.fenced) {
            Some((date, title)) => entries.push(WikiHistoryEntry {
                date,
                title,
                body: String::new(),
            }),
            None => {
                if let Some(entry) = entries.last_mut() {
                    entry.body.push_str(line.text);
                    entry.body.push('\n');
                }
            }
        }
    }
    for entry in &mut entries {
        entry.body = entry.body.trim().to_owned();
    }
    entries
}

fn history_heading(line: &str) -> Option<(Date, String)> {
    let heading = line.strip_prefix("###")?;
    let heading = heading.strip_prefix(char::is_whitespace)?.trim();
    let (date, rest) = heading.split_at_checked(10)?;
    let date = iso_date(date)?;
    if rest.trim().is_empty() {
        return Some((date, String::new()));
    }
    let title = rest
        .strip_prefix(char::is_whitespace)?
        .trim_start()
        .strip_prefix(['—', '–', '-'])?
        .strip_prefix(char::is_whitespace)?;
    Some((date, title.trim().to_owned()))
}
