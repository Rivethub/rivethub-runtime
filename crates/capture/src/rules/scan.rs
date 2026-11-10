use std::ops::Range;

use super::Allow;
use super::Decode;
use super::Matcher;
use super::Rule;
use super::Ruleset;
use super::Secret;
use super::Source;
use super::Target;
use super::decode::Budget;
use super::decode::Decoded;
use super::decode::decoded;
use super::value::shannon_entropy;

const WINDOW: usize = 64 * 1024;
const LOOKAHEAD_LINES: usize = 16;
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
        self.spans_within(text, &mut Budget::after(0))
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
                let start = line_start(text, hit.start());
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
        let mut kept: Vec<Span<'_>> = Vec::new();
        for span in found {
            if span.range.is_empty() {
                continue;
            }
            let overlaps = kept.iter().any(|taken| {
                span.range.start < taken.range.end && taken.range.start < span.range.end
            });
            if !overlaps {
                kept.push(span);
            }
        }
        kept.sort_by_key(|span| span.range.start);
        kept
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
        let global = match rule.source {
            Source::Rivethub => &self.allow_ours,
            Source::Gitleaks => &self.allow_theirs,
        };
        for captures in regex.captures_iter(slice) {
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let secret = match rule.secret {
                Secret::Whole => Some(whole),
                Secret::Group(group) => captures.get(group),
                Secret::FirstGroup => Some(
                    (1..captures.len())
                        .filter_map(|group| captures.get(group))
                        .find(|group| !group.is_empty())
                        .unwrap_or(whole),
                ),
            };
            let Some(secret) = secret.filter(|secret| !secret.is_empty()) else {
                continue;
            };
            if let Some(floor) = rule.entropy
                && shannon_entropy(secret.as_str()) <= floor
            {
                continue;
            }
            let line = line_of(slice, whole.start());
            let allowed = |allow: &Allow| allow.permits(secret.as_str(), whole.as_str(), line);
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

impl Ruleset {
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

fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |found| found + 1)
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

fn line_of(slice: &str, at: usize) -> &str {
    let start = line_start(slice, at);
    let end = line_end(slice, at);
    slice.get(start..end).unwrap_or(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_ends_on_a_char_boundary() {
        let text = format!("{}é", "x".repeat(WINDOW - 1));
        assert!(text.is_char_boundary(window_end(&text, 0)));
    }
}
