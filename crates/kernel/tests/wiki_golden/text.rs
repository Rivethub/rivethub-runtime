use std::convert::Infallible;

use kernel::wiki::page::{
    ArticlePatch, ArticleSection, ArticleSections, CappedSummary, PageCitation,
    apply_article_patches, cap_summary, demote_h2_headings, extract_wiki_links, find_stem_match,
    is_slug_variant, join_article_sections, merge_summary_text, normalize_slug, parse_citations,
    parse_see_also, split_article_sections,
};

use testkit::wiki_golden::{Tally, never_thrown};

use crate::{TestResult, corpus};

#[derive(serde::Deserialize)]
struct CapArgs(String, #[serde(default)] Option<usize>);

pub(crate) fn pure<A, O>(run: impl Fn(A) -> O) -> impl Fn(A) -> Result<O, Infallible> {
    move |input| Ok(run(input))
}

pub(crate) fn report(function: &str, result: Result<Tally, String>) -> TestResult {
    let tally = result.inspect_err(|failures| println!("{failures}"))?;
    println!(
        "{function}: {} passed, {} skipped",
        tally.passed, tally.skipped
    );
    Ok(())
}

#[test]
fn normalize_slug_replays() -> TestResult {
    report(
        "normalize_slug",
        corpus().replay(
            "normalize_slug",
            pure(|text: String| normalize_slug(&text)),
            never_thrown,
        ),
    )
}

#[test]
fn extract_wiki_links_replays() -> TestResult {
    report(
        "extract_wiki_links",
        corpus().replay(
            "extract_wiki_links",
            pure(|text: String| extract_wiki_links(&text)),
            never_thrown,
        ),
    )
}

#[test]
fn demote_h2_headings_replays() -> TestResult {
    report(
        "demote_h2_headings",
        corpus().replay(
            "demote_h2_headings",
            pure(|text: String| demote_h2_headings(&text)),
            never_thrown,
        ),
    )
}

#[test]
fn split_article_sections_replays() -> TestResult {
    report(
        "split_article_sections",
        corpus().replay(
            "split_article_sections",
            pure(|text: String| -> ArticleSections { split_article_sections(&text) }),
            never_thrown,
        ),
    )
}

#[test]
fn join_article_sections_replays() -> TestResult {
    report(
        "join_article_sections",
        corpus().replay(
            "join_article_sections",
            pure(|(lead, sections): (String, Vec<ArticleSection>)| {
                join_article_sections(&lead, &sections)
            }),
            never_thrown,
        ),
    )
}

#[test]
fn apply_article_patches_replays() -> TestResult {
    report(
        "apply_article_patches",
        corpus().replay(
            "apply_article_patches",
            pure(|(article, patches): (String, Vec<ArticlePatch>)| {
                apply_article_patches(&article, &patches)
            }),
            never_thrown,
        ),
    )
}

#[test]
fn parse_see_also_replays() -> TestResult {
    report(
        "parse_see_also",
        corpus().replay(
            "parse_see_also",
            pure(|text: String| parse_see_also(&text)),
            never_thrown,
        ),
    )
}

#[test]
fn parse_citations_replays() -> TestResult {
    report(
        "parse_citations",
        corpus().replay(
            "parse_citations",
            pure(|text: String| -> Vec<PageCitation> { parse_citations(&text) }),
            never_thrown,
        ),
    )
}

#[test]
fn merge_summary_text_replays() -> TestResult {
    report(
        "merge_summary_text",
        corpus().replay(
            "merge_summary_text",
            pure(|(existing, delta): (String, String)| merge_summary_text(&existing, &delta)),
            never_thrown,
        ),
    )
}

#[test]
fn cap_summary_replays() -> TestResult {
    report(
        "cap_summary",
        corpus().replay(
            "cap_summary",
            pure(|CapArgs(text, limit)| -> CappedSummary {
                cap_summary(&text, limit.unwrap_or(kernel::wiki::page::SUMMARY_LIMIT))
            }),
            never_thrown,
        ),
    )
}

#[test]
fn is_slug_variant_replays() -> TestResult {
    report(
        "is_slug_variant",
        corpus().replay(
            "is_slug_variant",
            pure(|(left, right): (String, String)| is_slug_variant(&left, &right)),
            never_thrown,
        ),
    )
}

#[test]
fn find_stem_match_replays() -> TestResult {
    report(
        "find_stem_match",
        corpus().replay(
            "find_stem_match",
            pure(|(proposed, existing): (String, Vec<String>)| {
                find_stem_match(&proposed, &existing)
            }),
            never_thrown,
        ),
    )
}
