use kernel::wiki::page::{
    PagePatch, ReadSection, WikiPage, WikiPageError, apply_patch, build_wiki_search_text,
    format_wiki_read, parse_wiki_page, serialize_wiki_page,
};

use testkit::wiki_golden::{Thrown, ThrownName, never_thrown};

use crate::text::report;
use crate::{TestResult, corpus};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    slug: String,
    #[serde(default)]
    section: Option<ReadSection>,
}

pub(crate) fn same_error(error: &WikiPageError, thrown: &Thrown) -> bool {
    match (error, thrown.name) {
        (WikiPageError::Invalid(_), ThrownName::WikiParseError) => {
            error.to_string() == thrown.message
        }
        (WikiPageError::Yaml(_), ThrownName::YamlParseError) => true,
        (WikiPageError::Invalid(_) | WikiPageError::Yaml(_) | WikiPageError::Emit(_), _) => false,
    }
}

#[test]
fn parse_wiki_page_replays() -> TestResult {
    report(
        "parse_wiki_page",
        corpus().replay(
            "parse_wiki_page",
            |markdown: String| parse_wiki_page(&markdown),
            same_error,
        ),
    )
}

#[test]
fn serialize_wiki_page_replays() -> TestResult {
    report(
        "serialize_wiki_page",
        corpus().replay(
            "serialize_wiki_page",
            |page: WikiPage| serialize_wiki_page(&page),
            never_thrown,
        ),
    )
}

#[test]
fn build_wiki_search_text_replays() -> TestResult {
    report(
        "build_wiki_search_text",
        corpus().replay(
            "build_wiki_search_text",
            crate::text::pure(|page: WikiPage| build_wiki_search_text(&page)),
            never_thrown,
        ),
    )
}

#[test]
fn apply_patch_replays() -> TestResult {
    report(
        "apply_patch",
        corpus().replay(
            "apply_patch",
            |(page, patch): (Option<WikiPage>, PagePatch)| apply_patch(page, patch),
            never_thrown,
        ),
    )
}

#[test]
fn format_wiki_read_replays() -> TestResult {
    report(
        "format_wiki_read",
        corpus().replay(
            "format_wiki_read",
            |(markdown, args): (String, ReadArgs)| {
                format_wiki_read(&markdown, &args.slug, args.section)
            },
            never_thrown,
        ),
    )
}
