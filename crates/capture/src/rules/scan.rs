use std::ops::Range;

use super::Allow;
use super::Rule;
use super::Ruleset;
use super::Target;

const WINDOW: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct Span<'a> {
    pub range: Range<usize>,
    pub rule: &'a Rule,
    order: usize,
}

impl Ruleset {
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span<'_>> {
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
                let end = if rule.multiline {
                    window_end(text, start)
                } else {
                    line_end(text, hit.end())
                };
                *done = end;
                let Some(slice) = text.get(start..end) else {
                    continue;
                };
                self.matches(rule, index, slice, start, &mut found);
            }
        }
        found.sort_by_key(|span| (span.range.start, span.order));
        let mut kept: Vec<Span<'_>> = Vec::new();
        for span in found {
            if span.range.is_empty() {
                continue;
            }
            if kept
                .last()
                .is_some_and(|last| span.range.start < last.range.end)
            {
                continue;
            }
            kept.push(span);
        }
        kept
    }

    fn matches<'a>(
        &'a self,
        rule: &'a Rule,
        order: usize,
        slice: &str,
        offset: usize,
        out: &mut Vec<Span<'a>>,
    ) {
        for captures in rule.regex.captures_iter(slice) {
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let secret = match rule.secret_group {
                Some(group) => captures.get(group),
                None => Some(
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
            if self.allow.iter().any(allowed) || rule.allow.iter().any(allowed) {
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

#[expect(
    clippy::cast_precision_loss,
    reason = "a secret is far shorter than 2^52 bytes, so the counts convert exactly"
)]
fn shannon_entropy(text: &str) -> f64 {
    let mut counts = [0usize; 256];
    for byte in text.bytes() {
        if let Some(slot) = counts.get_mut(usize::from(byte)) {
            *slot += 1;
        }
    }
    let total = text.len() as f64;
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let p = count as f64 / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_grows_with_variety() {
        assert!(shannon_entropy("aaaaaaaa") < 0.01);
        assert!(shannon_entropy("abcdefgh") > 2.9);
        assert!(shannon_entropy("Zq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA1e") > 4.5);
    }

    #[test]
    fn a_window_ends_on_a_char_boundary() {
        let text = format!("{}é", "x".repeat(WINDOW - 1));
        assert!(text.is_char_boundary(window_end(&text, 0)));
    }
}
