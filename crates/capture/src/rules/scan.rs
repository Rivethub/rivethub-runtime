use std::collections::BTreeMap;
use std::ops::Range;

use regex::Captures;
use regex::Match;

use super::Allow;
use super::Decode;
use super::Matcher;
use super::Rule;
use super::Ruleset;
use super::Secret;
use super::Source;
use super::Target;
use super::decode::Budget;
use super::decode::CANDIDATE_WORK;
use super::decode::Decoded;
use super::decode::SCAN_WORK;
use super::decode::SCANNED_BYTE_WORK;
use super::decode::decoded;
use super::value::shannon_entropy;

pub(super) const WINDOW: usize = 64 * 1024;
pub(super) const LOOKAHEAD_LINES: usize = 16;
const LOOKBEHIND: usize = WINDOW / 4;
const DECODE_DEPTH: usize = 2;

#[derive(Debug, Clone)]
pub struct Span<'a> {
    pub range: Range<usize>,
    pub rule: &'a Rule,
    order: usize,
}

#[derive(Debug, Clone, Copy)]
struct Scope {
    order: usize,
    offset: usize,
    depth: usize,
}

impl Ruleset {
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span<'_>> {
        self.spans_within(text, &mut Budget::fresh())
    }

    #[must_use]
    pub fn spans_of_rule(&self, id: &str, text: &str) -> Vec<Span<'_>> {
        let mut found = Vec::new();
        if let Some((index, rule)) = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, rule)| rule.id == id)
        {
            let scope = Scope {
                order: index,
                offset: 0,
                depth: 0,
            };
            self.matches(rule, text, scope, &mut Budget::fresh(), &mut found);
        }
        found
    }

    #[must_use]
    pub fn spans_within(&self, text: &str, budget: &mut Budget<'_>) -> Vec<Span<'_>> {
        self.scan(text, 0, budget)
    }

    fn scan(&self, text: &str, depth: usize, budget: &mut Budget<'_>) -> Vec<Span<'_>> {
        let mut scanned: Vec<usize> = vec![0; self.rules.len()];
        let mut found: Vec<Span<'_>> = Vec::new();
        for hit in self.keywords.find_overlapping_iter(text) {
            let Some(owners) = self.keyword_rules.get(hit.pattern().as_usize()) else {
                continue;
            };
            for &index in owners {
                let (Some(rule), Some(done)) = (self.rules.get(index), scanned.get_mut(index))
                else {
                    continue;
                };
                if hit.start() < *done {
                    continue;
                }
                let start = if rule.multiline {
                    window_start(text, hit.start())
                } else {
                    line_start(text, hit.start())
                };
                let line = line_end(text, hit.end());
                let end = if rule.multiline {
                    window_end(text, start)
                } else if matches!(rule.matcher, Matcher::Names) {
                    names_end(text, start, line)
                } else {
                    line
                };
                *done = if rule.multiline { end } else { line };
                let Some(slice) = text.get(start..end) else {
                    continue;
                };
                let scope = Scope {
                    order: index,
                    offset: start,
                    depth,
                };
                self.matches(rule, slice, scope, budget, &mut found);
            }
        }
        for &index in &self.always {
            if let Some(rule) = self.rules.get(index) {
                let scope = Scope {
                    order: index,
                    offset: 0,
                    depth,
                };
                self.matches(rule, text, scope, budget, &mut found);
            }
        }
        found.sort_by_key(|span| (span.rule.source, span.range.start, span.order));
        let mut kept: BTreeMap<usize, Span<'_>> = BTreeMap::new();
        for span in found {
            let overlaps = kept
                .range(..span.range.end)
                .next_back()
                .is_some_and(|(_, taken)| span.range.start < taken.range.end);
            if !span.range.is_empty() && !overlaps {
                kept.insert(span.range.start, span);
            }
        }
        kept.into_values().collect()
    }

    fn matches<'a>(
        &'a self,
        rule: &'a Rule,
        slice: &str,
        scope: Scope,
        budget: &mut Budget<'_>,
        out: &mut Vec<Span<'a>>,
    ) {
        let Scope {
            order,
            offset,
            depth,
        } = scope;
        let regex = match &rule.matcher {
            Matcher::Names | Matcher::Pairs => {
                let ranges = if matches!(rule.matcher, Matcher::Names) {
                    self.names.hits(slice)
                } else {
                    self.names.pair_hits(slice)
                };
                out.extend(ranges.into_iter().map(|range| Span {
                    range: offset + range.start..offset + range.end,
                    rule,
                    order,
                }));
                return;
            }
            Matcher::Budget => return,
            Matcher::Regex(regex) => regex,
        };
        let global = self.global_allowlist(rule.source);
        let mut line = CurrentLine::default();
        for captures in regex.captures_iter(slice) {
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(secret) = rule.secret_of(&captures, whole) else {
                continue;
            };
            if rule.decode == Some(Decode::Base64)
                && !budget.charge(CANDIDATE_WORK + secret.len() as u64)
            {
                break;
            }
            if let Some(floor) = rule.entropy
                && shannon_entropy(secret.as_str()) <= floor
            {
                continue;
            }
            let line_text = line.of(slice, whole.start());
            let allowed = |allow: &Allow| allow.permits(secret.as_str(), whole.as_str(), line_text);
            if global.iter().any(allowed) || rule.allow.iter().any(allowed) {
                continue;
            }
            let rule = if rule.decode == Some(Decode::Base64) {
                match self.hidden(rule, secret.as_str(), depth, budget) {
                    Some(rule) => rule,
                    None => continue,
                }
            } else {
                rule
            };
            if let Some(each) = &rule.each {
                for inner in each.captures_iter(secret.as_str()) {
                    if let Some(value) = inner.get(1).filter(|value| !value.is_empty()) {
                        out.push(Span {
                            range: offset + secret.start() + value.start()
                                ..offset + secret.start() + value.end(),
                            rule,
                            order,
                        });
                    }
                }
                continue;
            }
            out.push(Span {
                range: offset + secret.start()..offset + secret.end(),
                rule,
                order,
            });
        }
    }
}

impl Rule {
    fn secret_of<'s>(&self, captures: &Captures<'s>, whole: Match<'s>) -> Option<Match<'s>> {
        let secret = match self.secret {
            Secret::Whole => Some(whole),
            Secret::Group(group) => captures.get(group),
            Secret::FirstGroup => Some(
                (1..captures.len())
                    .filter_map(|group| captures.get(group))
                    .find(|group| !group.is_empty())
                    .unwrap_or(whole),
            ),
        };
        secret.filter(|secret| !secret.is_empty())
    }
}

impl Ruleset {
    fn global_allowlist(&self, source: Source) -> &[Allow] {
        match source {
            Source::Rivethub => &self.allow_ours,
            Source::Gitleaks => &self.allow_theirs,
        }
    }

    fn budget_rule<'a>(&'a self, fallback: &'a Rule) -> &'a Rule {
        self.rules.get(self.budget).unwrap_or(fallback)
    }

    fn hidden<'a>(
        &'a self,
        rule: &'a Rule,
        blob: &str,
        depth: usize,
        budget: &mut Budget<'_>,
    ) -> Option<&'a Rule> {
        let found = if depth < DECODE_DEPTH {
            decoded(blob, budget)?
        } else {
            Decoded::OverBudget
        };
        let undecoded = self.budget_rule(rule);
        match found {
            Decoded::Text(inner) => {
                if !budget.charge(SCAN_WORK + SCANNED_BYTE_WORK * inner.len() as u64) {
                    return Some(undecoded);
                }
                let inner = self.scan(&inner, depth + 1, budget);
                if inner.is_empty() {
                    None
                } else if inner.iter().all(|span| std::ptr::eq(span.rule, undecoded)) {
                    Some(undecoded)
                } else {
                    Some(rule)
                }
            }
            Decoded::DerPrivateKey => Some(rule),
            Decoded::OverBudget => Some(undecoded),
        }
    }
}

impl Allow {
    fn permits(&self, secret: &str, whole: &str, line: &str) -> bool {
        let target = match self.target {
            Target::Secret => secret,
            Target::Match => whole,
            Target::Line => line,
        };
        let by_regex =
            !self.regexes.is_empty() && self.regexes.iter().any(|re| re.is_match(target));
        let lowered = secret.to_lowercase();
        let by_word = !self.stopwords.is_empty()
            && self
                .stopwords
                .iter()
                .any(|word| lowered.contains(word.as_str()));
        if self.all {
            (self.regexes.is_empty() || by_regex) && (self.stopwords.is_empty() || by_word)
        } else {
            by_regex || by_word
        }
    }
}

#[derive(Default)]
struct CurrentLine(Option<Range<usize>>);

impl CurrentLine {
    fn of<'s>(&mut self, slice: &'s str, at: usize) -> &'s str {
        let range = match self.0.take() {
            Some(range) if range.start <= at && at <= range.end => range,
            _ => line_start(slice, at)..line_end(slice, at),
        };
        let line = slice.get(range.clone()).unwrap_or(slice);
        self.0 = Some(range);
        line
    }
}

fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |found| found + 1)
}

fn window_start(text: &str, at: usize) -> usize {
    let mut floor = at.saturating_sub(LOOKBEHIND);
    while !text.is_char_boundary(floor) {
        floor += 1;
    }
    text.get(floor..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(floor, |found| floor + found + 1)
}

fn line_end(text: &str, at: usize) -> usize {
    text.get(at..)
        .and_then(|tail| tail.find('\n'))
        .map_or(text.len(), |found| at + found)
}

fn names_end(text: &str, start: usize, line: usize) -> usize {
    let tail = text.get(start..line).unwrap_or_default().trim_end();
    let indicator = tail.rsplit_once(':').map(|(_, after)| after.trim());
    if matches!(indicator, Some("|" | "|-" | "|+" | ">" | ">-" | ">+")) {
        window_end(text, start)
    } else if tail.ends_with(['=', ':', '|', '>', '{', '['])
        || tail.ends_with("|-")
        || tail.ends_with(">-")
    {
        lookahead_end(text, line)
    } else {
        line
    }
}

fn lookahead_end(text: &str, line: usize) -> usize {
    let mut end = line;
    for _ in 0..LOOKAHEAD_LINES {
        match text.get(end + 1..).and_then(|tail| tail.find('\n')) {
            Some(found) => end += 1 + found,
            None => return text.len(),
        }
    }
    end
}

fn window_end(text: &str, start: usize) -> usize {
    let mut end = start.saturating_add(WINDOW).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(text: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn fan_out(bytes: usize) -> String {
        let token = blob("AAAAAA");
        blob(&format!("{token} ").repeat(bytes / 4 * 3 / (token.len() + 1)))
    }

    #[test]
    fn nested_candidates_spend_a_small_work_allowance_and_stop_the_scan() {
        let rules = crate::rules::injection::ruleset();
        let text = fan_out(256 << 10);
        let mut budget = Budget::fresh().with_work(1 << 20);
        let spans = rules.spans_within(&text, &mut budget);
        assert!(budget.exhausted());
        assert!(!spans.is_empty());
        let mut again = Budget::fresh().with_work(1 << 20);
        let repeat = rules.spans_within(&text, &mut again);
        assert_eq!(budget.spent(), again.spent());
        assert_eq!(spans.len(), repeat.len());
    }

    #[test]
    fn ordinary_text_leaves_the_work_allowance_nearly_whole() {
        let rules = crate::rules::injection::ruleset();
        let text =
            "Configuration persistence serialization, authentication tokens. ".repeat(16 << 10);
        let mut budget = Budget::fresh();
        assert!(rules.spans_within(&text, &mut budget).is_empty());
        assert!(!budget.exhausted());
        assert!(budget.spent().work <= 4 * text.len() as u64);
    }

    #[test]
    fn a_window_ends_on_a_char_boundary() {
        let text = format!("{}é", "x".repeat(WINDOW - 1));
        assert!(text.is_char_boundary(window_end(&text, 0)));
    }
}
