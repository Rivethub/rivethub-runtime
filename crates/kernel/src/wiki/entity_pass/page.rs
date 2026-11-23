use super::types::FactState;
use super::types::LiveFact;
use values::time::LegacyStrftime as _;

#[must_use]
pub fn prose_article(article: &str) -> String {
    let trimmed = article.trim();
    if let Some((prose, _)) = trimmed.split_once("\n### Current facts\n") {
        return prose.trim().to_owned();
    }
    if trimmed == "### Current facts" || trimmed.starts_with("### Current facts\n") {
        return String::new();
    }
    trimmed.to_owned()
}

#[must_use]
pub fn facts_section(facts: &[LiveFact]) -> String {
    let mut sorted: Vec<&LiveFact> = facts.iter().collect();
    sorted.sort_by(|a, b| a.attribute.cmp(&b.attribute).then(b.as_of.cmp(&a.as_of)));
    let mut out = String::from("### Current facts\n");
    for fact in sorted {
        let ended = if fact.status == FactState::Ended {
            " (ended)"
        } else {
            ""
        };
        out.push_str("\n- **");
        out.push_str(fact.attribute.trim());
        out.push_str("**: ");
        out.push_str(fact.value.trim());
        out.push_str(ended);
        out.push_str(" (as of ");
        out.push_str(&fact.as_of.legacy_strftime("%Y-%m-%d"));
        out.push(')');
    }
    out
}

#[must_use]
pub fn article_with_facts(article: &str, facts: &[LiveFact]) -> String {
    let article = article.trim();
    if facts.is_empty() {
        return article.to_owned();
    }
    let section = facts_section(facts);
    if article.is_empty() {
        section
    } else {
        format!("{article}\n\n{section}")
    }
}

use super::{EntityProse, EntityStoreError, EntityView, render_history};
use crate::wiki::page::LastVerified;
use crate::wiki::page::PageCitation;
use crate::wiki::page::PageSource;
use crate::wiki::page::WikiPage;
use crate::wiki::page::WikiSourceKind;

pub fn entity_page(view: &EntityView, prose: &EntityProse) -> Result<WikiPage, EntityStoreError> {
    let slug = crate::wiki::page::WikiSlug::try_from(view.slug.clone())
        .map_err(|_| EntityStoreError::SlugTaken(view.slug.clone()))?;
    let mut page = WikiPage::empty(slug, view.name.clone());
    prose.summary.trim().clone_into(&mut page.current_state);
    page.article = article_with_facts(&prose.article, &view.facts);
    page.meta.aliases.clone_from(&view.aliases);
    page.meta.tags = page_tags(view);
    page.history = render_history(&view.history);
    page.meta.last_verified = view
        .facts
        .iter()
        .map(|fact| fact.as_of)
        .max()
        .map(LastVerified::from);
    page.meta.sources = page_sources(view);
    page.citations = view
        .history
        .iter()
        .map(|line| PageCitation {
            summary_id: line.summary_id.to_string(),
            date: Some(line.date),
            kind: None,
            note: None,
        })
        .collect();
    drop(crate::wiki::page::bound::bound_page(None, &mut page));
    Ok(page)
}

fn page_tags(view: &EntityView) -> Vec<String> {
    let mut tags = Vec::new();
    if !view.type_slug.is_empty() {
        tags.push(view.type_slug.clone());
    }
    if !view.type_name.is_empty() && !tags.iter().any(|tag| tag == &view.type_name) {
        tags.push(view.type_name.clone());
    }
    tags
}

fn page_sources(view: &EntityView) -> Vec<PageSource> {
    let mut ids: Vec<String> = view
        .history
        .iter()
        .map(|line| line.summary_id.to_string())
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Vec::new();
    }
    vec![PageSource {
        kind: WikiSourceKind::Summary,
        ids,
        conversation_id: view
            .history
            .iter()
            .find_map(|line| line.conversation_id)
            .map(|id| id.to_string()),
        extra: values::Extra::default(),
    }]
}
