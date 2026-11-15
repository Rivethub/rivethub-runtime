use super::{RecompileReport, SourceSummary};
use crate::wiki::merge::{MergeInput, Merged, PageMerger};
use crate::wiki::page::bound::{ARTICLE_BUDGET, SECTION_BUDGET, fit, tail};
use crate::wiki::page::{SUMMARY_LIMIT, WikiPage, cap_summary, demote_h2_headings};

const ARTICLE_LEAD: usize = 4 * 1024;
const ARTICLE_MATERIAL: usize = 12 * 1024;
const SECTION_LEAD: usize = 2 * 1024;
const SECTION_MATERIAL: usize = 8 * 1024;
const PAGE_TEXT: usize = 12 * 1024;
const ITEM: usize = 3 * 1024;
const ARTICLE_LIMIT: usize = 14_000;
const SECTION_LIMIT: usize = 3_000;
const SECTION_CALLS: usize = 32;

pub(super) async fn consolidate(
    page: &mut WikiPage,
    summaries: &[SourceSummary],
    pages: &dyn PageMerger,
    report: &mut RecompileReport,
) {
    let owned: Vec<(String, String)> = page
        .meta
        .sections
        .iter()
        .map(|known| (known.topic.clone(), known.heading.clone()))
        .collect();
    let loose: Vec<&SourceSummary> = summaries
        .iter()
        .filter(|item| {
            item.topic
                .as_ref()
                .is_none_or(|topic| !owned.iter().any(|(known, _)| known == topic))
        })
        .collect();
    article(page, &loose, pages, report).await;
    sections(page, &owned, summaries, pages, report).await;
    lead(page, pages, report).await;
}

async fn article(
    page: &mut WikiPage,
    loose: &[&SourceSummary],
    pages: &dyn PageMerger,
    report: &mut RecompileReport,
) {
    let fresh = !loose.is_empty() && !page.article.trim().is_empty();
    if page.article.len() <= ARTICLE_BUDGET && !fresh {
        return;
    }
    let material = if loose.is_empty() {
        vec![tail(&page.article, ARTICLE_MATERIAL)]
    } else {
        newest(loose, ARTICLE_MATERIAL)
    };
    let input = MergeInput {
        title: page.meta.title.clone(),
        summary: cap_summary(&page.current_state, SUMMARY_LIMIT).kept,
        article: fit(&page.article, ARTICLE_LEAD).kept,
        material,
        limit: ARTICLE_LIMIT,
        summary_only: false,
    };
    match ask(pages, &input, report).await {
        Some(merged) if !merged.article.trim().is_empty() => {
            if !merged.summary.trim().is_empty() {
                page.current_state = demote_h2_headings(merged.summary.trim());
            }
            page.article = demote_h2_headings(merged.article.trim());
        }
        Some(_) | None => {}
    }
}

async fn sections(
    page: &mut WikiPage,
    owned: &[(String, String)],
    summaries: &[SourceSummary],
    pages: &dyn PageMerger,
    report: &mut RecompileReport,
) {
    let title = page.meta.title.clone();
    let mut order: Vec<(usize, Vec<&SourceSummary>)> = page
        .extra_sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let topic = owned
                .iter()
                .find(|(_, heading)| *heading == section.heading)
                .map(|(topic, _)| topic.as_str());
            let mine = summaries
                .iter()
                .filter(|item| topic.is_some() && item.topic.as_deref() == topic)
                .collect();
            (index, mine)
        })
        .filter(|(index, mine): &(usize, Vec<&SourceSummary>)| {
            !mine.is_empty()
                || page
                    .extra_sections
                    .get(*index)
                    .is_some_and(|section| section.body.len() > SECTION_BUDGET)
        })
        .collect();
    order.sort_by_key(|(_, mine)| std::cmp::Reverse(mine.first().map(|item| item.at)));
    for (index, mine) in order.into_iter().take(SECTION_CALLS) {
        let Some(section) = page.extra_sections.get_mut(index) else {
            continue;
        };
        let material = if mine.is_empty() {
            vec![tail(&section.body, SECTION_MATERIAL)]
        } else {
            newest(&mine, SECTION_MATERIAL)
        };
        let input = MergeInput {
            title: format!("{title} › {}", section.heading),
            summary: String::new(),
            article: fit(&section.body, SECTION_LEAD).kept,
            material,
            limit: SECTION_LIMIT,
            summary_only: false,
        };
        if let Some(merged) = ask(pages, &input, report).await {
            section.body = demote_h2_headings(
                &[merged.summary.trim(), merged.article.trim()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
        }
    }
}

async fn lead(page: &mut WikiPage, pages: &dyn PageMerger, report: &mut RecompileReport) {
    if !page.current_state.trim().is_empty() && report.merges == 0 {
        return;
    }
    let mut text = vec![page.article.trim().to_owned()];
    text.extend(
        page.extra_sections
            .iter()
            .filter(|section| !section.body.trim().is_empty())
            .map(|section| format!("### {}\n\n{}", section.heading, section.body.trim())),
    );
    let input = MergeInput {
        title: page.meta.title.clone(),
        summary: cap_summary(&page.current_state, SUMMARY_LIMIT).kept,
        article: fit(&text.join("\n\n"), PAGE_TEXT).kept,
        material: Vec::new(),
        limit: 0,
        summary_only: true,
    };
    if let Some(merged) = ask(pages, &input, report).await
        && !merged.summary.trim().is_empty()
    {
        page.current_state = demote_h2_headings(merged.summary.trim());
    }
}

fn newest(items: &[&SourceSummary], budget: usize) -> Vec<String> {
    let mut used = 0usize;
    let mut material = Vec::new();
    for item in items {
        let text = format!(
            "{}: {}",
            item.at.format("%Y-%m-%d %H:%M"),
            fit(&item.content, ITEM).kept
        );
        if used.saturating_add(text.len()) > budget && !material.is_empty() {
            break;
        }
        used = used.saturating_add(text.len());
        material.push(text);
    }
    material
}

async fn ask(
    pages: &dyn PageMerger,
    input: &MergeInput,
    report: &mut RecompileReport,
) -> Option<Merged> {
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let merged = pages.merge(input).await.ok();
    let merged = merged
        .filter(|merged| !merged.summary.trim().is_empty() || !merged.article.trim().is_empty());
    if merged.is_some() {
        report.merges = report.merges.saturating_add(1);
    } else {
        report.merge_failures = report.merge_failures.saturating_add(1);
    }
    merged
}
