use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use values::time::TimestampFormat as _;

use super::article::{ArticlePatch, apply_article_patches, demote_h2_headings};
use super::limits;
use super::slug::{extract_wiki_links, normalize_slug};
use super::summary::{SUMMARY_LIMIT, cap_summary, merge_summary_text};
use super::{
    LastVerified, PageCitation, PageSource, WikiAction, WikiHistoryEntry, WikiPage, WikiSlug,
    WikiSlugError,
};

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PagePatch {
    pub action: WikiAction,
    pub slug: String,
    #[serde_as(as = "values::Millis")]
    pub verified_at: Timestamp,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub add_aliases: Vec<String>,
    #[serde(default)]
    pub add_tags: Vec<String>,
    #[serde(default)]
    pub add_entities: Vec<String>,
    #[serde(default)]
    pub add_related: Vec<String>,
    #[serde(default)]
    pub summary_delta: Option<String>,
    #[serde(default)]
    pub current_state: Option<String>,
    #[serde(default)]
    pub article: Option<String>,
    #[serde(default)]
    pub article_patches: Vec<ArticlePatch>,
    #[serde(default)]
    pub history_entry: Option<WikiHistoryEntry>,
    #[serde(default)]
    pub add_citations: Vec<PageCitation>,
    #[serde(default)]
    pub add_sources: Vec<PageSource>,
    #[serde(default)]
    pub allow_shrink: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::EnumString)]
pub(super) enum Revision {
    #[strum(serialize = "Superseded current state")]
    CurrentState,
    #[strum(serialize = "Summary overflow (v7 cap)")]
    SummaryOverflow,
    #[strum(serialize = "Superseded article")]
    Article,
}

pub fn apply_patch(page: Option<WikiPage>, patch: PagePatch) -> Result<WikiPage, WikiSlugError> {
    let mut page = match page {
        Some(page) => page,
        None => new_page(&patch.slug)?,
    };
    let today = patch.verified_at.utc_date();
    if let Some(title) = patch
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        title.clone_into(&mut page.meta.title);
    }
    merge_list(&mut page.meta.aliases, patch.add_aliases, limits::ALIASES);
    merge_list(&mut page.meta.tags, patch.add_tags, limits::TAGS);
    merge_list(&mut page.meta.entities, patch.add_entities, usize::MAX);
    let mut revisions = revise_current_state(
        &mut page,
        patch.summary_delta,
        patch.current_state,
        patch.allow_shrink,
    );
    revisions.extend(revise_article(
        &mut page,
        patch.article,
        patch.article_patches,
    ));
    let entries = revisions
        .into_iter()
        .map(|(revision, body)| WikiHistoryEntry {
            date: today,
            title: revision.to_string(),
            body,
        })
        .chain(patch.history_entry.map(|entry| WikiHistoryEntry {
            body: demote_h2_headings(entry.body.trim()),
            ..entry
        }));
    for entry in entries {
        if !page.history.contains(&entry) {
            page.history.insert(0, entry);
        }
    }
    page.history.truncate(limits::HISTORY);
    for citation in patch.add_citations {
        if !citation.summary_id.trim().is_empty()
            && !page
                .citations
                .iter()
                .any(|existing| existing.summary_id == citation.summary_id)
        {
            page.citations.insert(0, citation);
        }
    }
    page.citations.truncate(limits::CITATIONS);
    for source in patch.add_sources {
        if !page.meta.sources.contains(&source) {
            page.meta.sources.push(source);
        }
    }
    relate(&mut page, &normalize_slug(&patch.slug), &patch.add_related);
    page.meta.last_verified = Some(LastVerified::from(patch.verified_at));
    Ok(page)
}

fn revise_current_state(
    page: &mut WikiPage,
    delta: Option<String>,
    replacement: Option<String>,
    allow_shrink: bool,
) -> Vec<(Revision, String)> {
    let mut revisions = Vec::new();
    if let Some(delta) = delta {
        page.current_state =
            merge_summary_text(&page.current_state, &demote_h2_headings(delta.trim()));
    }
    if let Some(replacement) = replacement {
        let replacement = demote_h2_headings(replacement.trim());
        if shrinks(&page.current_state, &replacement) && !allow_shrink {
            page.current_state = merge_summary_text(&page.current_state, &replacement);
        } else {
            let previous = std::mem::replace(&mut page.current_state, replacement);
            if !previous.trim().is_empty() && previous.trim() != page.current_state {
                revisions.push((Revision::CurrentState, previous));
            }
        }
    }
    let capped = cap_summary(&page.current_state, SUMMARY_LIMIT);
    page.current_state = capped.kept;
    if !capped.overflow.is_empty() {
        revisions.push((Revision::SummaryOverflow, capped.overflow));
    }
    revisions
}

fn revise_article(
    page: &mut WikiPage,
    replacement: Option<String>,
    patches: Vec<ArticlePatch>,
) -> Option<(Revision, String)> {
    let mut revision = None;
    if let Some(replacement) = replacement {
        let replacement = demote_h2_headings(replacement.trim());
        let previous = std::mem::replace(&mut page.article, replacement);
        if !previous.trim().is_empty() && previous.trim() != page.article {
            revision = Some((Revision::Article, previous));
        }
    }
    if !patches.is_empty() {
        let patches: Vec<ArticlePatch> = patches
            .into_iter()
            .map(|patch| ArticlePatch {
                body: demote_h2_headings(&patch.body),
                ..patch
            })
            .collect();
        page.article = apply_article_patches(&page.article, &patches);
    }
    revision
}

fn relate(page: &mut WikiPage, target: &str, additions: &[String]) {
    let links: Vec<String> = additions
        .iter()
        .map(|slug| normalize_slug(slug))
        .chain(extract_wiki_links(&page.current_state))
        .chain(extract_wiki_links(&page.article))
        .collect();
    merge_list(&mut page.meta.related, links, usize::MAX);
    page.meta.related.retain(|slug| slug != target);
    page.meta.related.truncate(limits::RELATED);
    let related = page.meta.related.clone();
    merge_list(&mut page.see_also, related, usize::MAX);
    page.see_also.retain(|slug| slug != target);
    page.see_also.truncate(limits::RELATED);
}

fn new_page(slug: &str) -> Result<WikiPage, WikiSlugError> {
    let slug = WikiSlug::try_from(normalize_slug(slug))?;
    let title = slug.as_ref().to_owned();
    Ok(WikiPage::empty(slug, title))
}

fn shrinks(previous: &str, next: &str) -> bool {
    !next.is_empty() && next.chars().count() * 5 < previous.trim().chars().count() * 3
}

pub(super) fn merge_list(
    target: &mut Vec<String>,
    items: impl IntoIterator<Item = String>,
    limit: usize,
) {
    let existing = std::mem::take(target);
    for item in existing.into_iter().chain(items) {
        if !item.is_empty() && !target.contains(&item) {
            target.push(item);
        }
    }
    target.truncate(limit);
}
