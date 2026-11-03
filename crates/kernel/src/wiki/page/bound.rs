use std::collections::BTreeSet;

use super::lines::fence_aware;
use super::patch::Revision;
use super::summary::{SUMMARY_LIMIT, cap_summary};
use super::{ArticleSection, WikiHistoryEntry, WikiPage};

pub const ARTICLE_BUDGET: usize = 16 * 1024;
pub const SECTION_BUDGET: usize = 4 * 1024;
pub const SECTIONS_BUDGET: usize = 12 * 1024;
pub const HISTORY_LINE: usize = 200;
const SECTION_FLOOR: usize = 512;
pub const ENTITIES: usize = 64;
pub const ARCHIVED_TITLE: &str = "Archived";
const ELLIPSIS: char = '…';
const COVERED_PERCENT: usize = 60;
const REPEAT_FLOOR: usize = 40;
const REPEAT_WORDS: usize = 12;
const REPEAT_PERCENT: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    pub label: String,
    pub text: String,
    pub notice: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitted {
    pub kept: String,
    pub overflow: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeBase {
    pub summary: String,
    pub article: String,
}

#[must_use]
pub fn is_revision(title: &str) -> bool {
    title.parse::<Revision>().is_ok()
}

#[must_use]
pub fn blocks(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in fence_aware(text) {
        if !line.fenced && line.text.trim().is_empty() {
            push_block(&mut found, &mut current);
        } else {
            current.push(line.text);
        }
    }
    push_block(&mut found, &mut current);
    found
}

fn push_block(found: &mut Vec<String>, current: &mut Vec<&str>) {
    let block = current.join("\n");
    let block = block.trim();
    if !block.is_empty() {
        found.push(block.to_owned());
    }
    current.clear();
}

fn normal(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn fit(text: &str, budget: usize) -> Fitted {
    let text = text.trim();
    if text.len() <= budget {
        return Fitted {
            kept: text.to_owned(),
            overflow: String::new(),
        };
    }
    let found = blocks(text);
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut rest = found.len();
    for (index, block) in found.iter().enumerate() {
        let cost = block
            .len()
            .saturating_add(if kept.is_empty() { 0 } else { 2 });
        if used.saturating_add(cost) > budget {
            rest = index;
            break;
        }
        used = used.saturating_add(cost);
        kept.push(block.clone());
    }
    let mut overflow: Vec<String> = found.iter().skip(rest).cloned().collect();
    if kept.is_empty()
        && let Some(first) = overflow.first().cloned()
    {
        let cut = cut_point(&first, budget);
        let (head, tail) = first.split_at(cut);
        kept.push(head.trim_end().to_owned());
        if let Some(slot) = overflow.first_mut() {
            tail.trim_start().clone_into(slot);
        }
    }
    overflow.retain(|block| !block.is_empty());
    Fitted {
        kept: kept.join("\n\n"),
        overflow: overflow.join("\n\n"),
    }
}

#[must_use]
pub fn tail(text: &str, budget: usize) -> String {
    let text = text.trim();
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for block in blocks(text).into_iter().rev() {
        let cost = block.len().saturating_add(2);
        if used.saturating_add(cost) > budget {
            if kept.is_empty() {
                let mut start = block.len().saturating_sub(budget);
                while start < block.len() && !block.is_char_boundary(start) {
                    start = start.saturating_add(1);
                }
                kept.push(
                    block
                        .get(start..)
                        .unwrap_or_default()
                        .trim_start()
                        .to_owned(),
                );
            }
            break;
        }
        used = used.saturating_add(cost);
        kept.push(block);
    }
    kept.reverse();
    kept.join("\n\n")
}

fn cut_point(text: &str, budget: usize) -> usize {
    let mut cut = budget.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let line = text
        .get(..cut)
        .and_then(|head| head.rfind('\n'))
        .filter(|at| at.saturating_mul(2) >= cut);
    line.unwrap_or(cut)
}

#[must_use]
pub fn dropped(before: &str, after: &str) -> String {
    missing(before, &normal(after))
}

fn missing(before: &str, after: &str) -> String {
    blocks(before)
        .into_iter()
        .filter(|block| !after.contains(&normal(block)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[must_use]
pub fn rebase(base: &str, ours: &str, current: &str) -> String {
    if normal(base) == normal(current) {
        return ours.trim().to_owned();
    }
    let since = Since::new(base, ours);
    let ours_blocks = blocks(ours);
    let ours_normal: BTreeSet<String> = ours_blocks.iter().map(|block| normal(block)).collect();
    let mut merged: Vec<String> = blocks(current)
        .into_iter()
        .filter(|block| ours_normal.contains(&normal(block)) || since.fresh(block))
        .collect();
    let mut anchor: Option<String> = None;
    for block in &ours_blocks {
        let key = normal(block);
        if !merged.iter().any(|kept| normal(kept) == key) {
            let at = anchor
                .as_ref()
                .and_then(|before| merged.iter().position(|kept| normal(kept) == *before))
                .map_or(0, |index| index.saturating_add(1));
            merged.insert(at.min(merged.len()), block.clone());
        }
        anchor = Some(key);
    }
    merged.join("\n\n")
}

#[must_use]
pub fn added_since(base: &str, current: &str, ours: &str) -> String {
    let since = Since::new(base, ours);
    blocks(current)
        .into_iter()
        .filter(|block| since.fresh(block))
        .collect::<Vec<_>>()
        .join("\n\n")
}

struct Since {
    base: BTreeSet<String>,
    said: BTreeSet<String>,
}

impl Since {
    fn new(base: &str, ours: &str) -> Self {
        Self {
            base: blocks(base).iter().map(|block| normal(block)).collect(),
            said: words(ours),
        }
    }

    fn fresh(&self, block: &str) -> bool {
        !self.base.contains(&normal(block)) && !covered(&words(block), &self.said)
    }
}

pub(super) fn prose(block: &str) -> bool {
    !block.starts_with(['#', '-', '*', '|', '>', '`'])
        && !block.starts_with(|letter: char| letter.is_ascii_digit())
}

pub(super) fn words(text: &str) -> BTreeSet<String> {
    text.split(|letter: char| !letter.is_alphanumeric() && !"_.#/-".contains(letter))
        .map(|word| word.trim_matches(|letter: char| !letter.is_alphanumeric()))
        .filter(|word| {
            word.chars().count() >= 4 || word.chars().any(|letter| letter.is_ascii_digit())
        })
        .map(str::to_lowercase)
        .collect()
}

pub(super) fn covered(found: &BTreeSet<String>, pool: &BTreeSet<String>) -> bool {
    overlaps(found, pool, COVERED_PERCENT)
}

pub(super) fn overlaps(found: &BTreeSet<String>, pool: &BTreeSet<String>, percent: usize) -> bool {
    let shared = found.iter().filter(|word| pool.contains(*word)).count();
    shared.saturating_mul(100) >= found.len().saturating_mul(percent)
}

#[must_use]
pub fn history_line(text: &str) -> Fitted {
    let text = text.trim();
    if text.chars().count() <= HISTORY_LINE {
        return Fitted {
            kept: text.to_owned(),
            overflow: String::new(),
        };
    }
    let first = text.lines().next().unwrap_or_default().trim();
    let limit = HISTORY_LINE.saturating_sub(1);
    let mut kept: String = first.chars().take(limit).collect();
    if first.chars().count() > limit {
        let sentence = kept
            .rfind(". ")
            .filter(|at| at.saturating_mul(2) >= kept.len());
        if let Some(at) = sentence {
            kept.truncate(at.saturating_add(1));
        } else {
            let trimmed = kept.trim_end().len();
            kept.truncate(trimmed);
            kept.push(ELLIPSIS);
        }
    }
    Fitted {
        kept,
        overflow: text.to_owned(),
    }
}

#[must_use]
pub fn bound_page(previous: Option<&WikiPage>, page: &mut WikiPage) -> Vec<Archived> {
    let mut archived = Vec::new();
    if let Some(previous) = previous {
        displaced(previous, page, &mut archived);
    }
    revisions(previous, page, &mut archived);
    let owned: Vec<String> = page
        .meta
        .sections
        .iter()
        .map(|known| known.heading.clone())
        .collect();
    archived.extend(bound_parts(
        &mut page.current_state,
        &mut page.article,
        &mut page.extra_sections,
        &owned,
    ));
    page.extra_sections
        .retain(|section| !section.body.trim().is_empty() || owned.contains(&section.heading));
    let excess = page.meta.entities.len().saturating_sub(ENTITIES);
    if excess > 0 {
        let names: Vec<String> = page.meta.entities.drain(..excess).collect();
        archived.push(Archived {
            label: "Entities".to_owned(),
            text: names.join(", "),
            notice: false,
        });
    }
    for entry in &mut page.history {
        let line = history_line(&entry.body);
        if line.overflow.is_empty() {
            continue;
        }
        let legacy = previous.is_some_and(|previous| previous.history.contains(entry));
        if legacy {
            spill(&mut archived, entry);
        }
        entry.body = line.kept;
    }
    archived
}

pub fn bound_parts(
    summary: &mut String,
    article: &mut String,
    sections: &mut [ArticleSection],
    owned: &[String],
) -> Vec<Archived> {
    let mut archived = Vec::new();
    let capped = cap_summary(summary, SUMMARY_LIMIT);
    *summary = capped.kept;
    keep(&mut archived, "Summary", &capped.overflow);
    drop_repeats(article);
    for section in sections.iter_mut() {
        drop_repeats(&mut section.body);
    }
    let fitted = fit(article, ARTICLE_BUDGET);
    *article = fitted.kept;
    keep(&mut archived, "Article", &fitted.overflow);
    let spare = ARTICLE_BUDGET.saturating_sub(article.len());
    bound_sections(
        sections,
        owned,
        SECTIONS_BUDGET.saturating_add(spare),
        &mut archived,
    );
    archived
}

fn drop_repeats(text: &mut String) {
    let found = blocks(text);
    let mut seen = Repeats::default();
    let kept: Vec<&str> = found
        .iter()
        .filter(|block| !seen.repeats(block))
        .map(String::as_str)
        .collect();
    if kept.len() < found.len() {
        *text = kept.join("\n\n");
    }
}

#[derive(Default)]
struct Repeats {
    exact: BTreeSet<String>,
    prose: Vec<BTreeSet<String>>,
}

impl Repeats {
    fn repeats(&mut self, block: &str) -> bool {
        if block.starts_with('#') || block.len() < REPEAT_FLOOR {
            return false;
        }
        if !self.exact.insert(normal(block)) {
            return true;
        }
        if !prose(block) {
            return false;
        }
        let found = words(block);
        let repeated = found.len() >= REPEAT_WORDS
            && self
                .prose
                .iter()
                .any(|earlier| overlaps(&found, earlier, REPEAT_PERCENT));
        if !repeated {
            self.prose.push(found);
        }
        repeated
    }
}

pub fn bound_sections(
    sections: &mut [ArticleSection],
    owned: &[String],
    budget: usize,
    archived: &mut Vec<Archived>,
) {
    for section in sections.iter_mut() {
        let fitted = fit(&section.body, SECTION_BUDGET);
        section.body = fitted.kept;
        keep(archived, &section.heading, &fitted.overflow);
    }
    loop {
        let sizes = sections.iter().map(|section| section.body.len()).collect();
        if share(sizes, budget) >= SECTION_FLOOR {
            break;
        }
        let Some(oldest) = sections
            .iter_mut()
            .find(|section| !section.body.is_empty() && !owned.contains(&section.heading))
        else {
            break;
        };
        let body = std::mem::take(&mut oldest.body);
        keep(archived, &oldest.heading, &body);
    }
    let cap = share(
        sections.iter().map(|section| section.body.len()).collect(),
        budget,
    );
    for section in sections
        .iter_mut()
        .filter(|section| section.body.len() > cap)
    {
        let fitted = fit(&section.body, cap);
        section.body = fitted.kept;
        keep(archived, &section.heading, &fitted.overflow);
    }
}

fn share(mut sizes: Vec<usize>, budget: usize) -> usize {
    sizes.sort_unstable();
    let mut left = budget;
    let count = sizes.len();
    for (index, size) in sizes.into_iter().enumerate() {
        let fair = left / count.saturating_sub(index).max(1);
        if size > fair {
            return fair;
        }
        left = left.saturating_sub(size);
    }
    usize::MAX
}

fn displaced(previous: &WikiPage, page: &WikiPage, archived: &mut Vec<Archived>) {
    let mut after = vec![page.current_state.as_str(), page.article.as_str()];
    after.extend(
        page.extra_sections
            .iter()
            .map(|section| section.body.as_str()),
    );
    let after = normal(&after.join("\n\n"));
    keep(archived, "Article", &missing(&previous.article, &after));
    for section in &previous.extra_sections {
        keep(archived, &section.heading, &missing(&section.body, &after));
    }
    for entry in &previous.history {
        if !is_revision(&entry.title) && !page.history.contains(entry) {
            spill(archived, entry);
        }
    }
}

fn revisions(previous: Option<&WikiPage>, page: &mut WikiPage, archived: &mut Vec<Archived>) {
    let entries = std::mem::take(&mut page.history);
    for entry in entries {
        if !is_revision(&entry.title) {
            page.history.push(entry);
            continue;
        }
        let legacy = previous.is_some_and(|previous| previous.history.contains(&entry));
        let fresh_overflow =
            !legacy && entry.title.parse::<Revision>() == Ok(Revision::SummaryOverflow);
        if legacy || fresh_overflow {
            keep(
                archived,
                &format!("{} ({})", entry.title, entry.date),
                &entry.body,
            );
        }
    }
}

fn keep(archived: &mut Vec<Archived>, label: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    archived.push(Archived {
        label: label.to_owned(),
        text: text.to_owned(),
        notice: true,
    });
}

pub(super) fn spill(archived: &mut Vec<Archived>, entry: &WikiHistoryEntry) {
    let body = entry.body.trim();
    if body.is_empty() || entry.title == ARCHIVED_TITLE {
        return;
    }
    archived.push(Archived {
        label: format!("History {} — {}", entry.date, entry.title),
        text: body.to_owned(),
        notice: false,
    });
}
