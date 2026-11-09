use std::ops::Range;

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::GeneralPurpose;
use base64::engine::GeneralPurposeConfig;

use super::Allow;
use super::Decode;
use super::Matcher;
use super::Rule;
use super::Ruleset;
use super::Secret;
use super::Source;
use super::Target;
use super::names::shannon_entropy;

const WINDOW: usize = 64 * 1024;
const PEM_OPEN: &str = "-----BEGIN";

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
    decode: bool,
}

impl Ruleset {
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span<'_>> {
        self.scan(text, true)
    }

    fn scan(&self, text: &str, decode: bool) -> Vec<Span<'_>> {
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
                let scope = Scope {
                    order: index,
                    offset: start,
                    decode,
                };
                self.matches(rule, slice, scope, &mut found);
            }
        }
        for &index in &self.always {
            if let Some(rule) = self.rules.get(index) {
                let scope = Scope {
                    order: index,
                    offset: 0,
                    decode,
                };
                self.matches(rule, text, scope, &mut found);
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

    fn matches<'a>(&'a self, rule: &'a Rule, slice: &str, scope: Scope, out: &mut Vec<Span<'a>>) {
        let Scope {
            order,
            offset,
            decode,
        } = scope;
        let regex = match &rule.matcher {
            Matcher::Names => {
                for range in self.names.hits(slice) {
                    out.push(Span {
                        range: offset + range.start..offset + range.end,
                        rule,
                        order,
                    });
                }
                return;
            }
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
            if rule.decode == Some(Decode::Base64) {
                let hidden = decode
                    && decoded_text(secret.as_str())
                        .is_some_and(|inner| !self.scan(&inner, false).is_empty());
                if !hidden {
                    continue;
                }
            }
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

fn decoded_text(blob: &str) -> Option<String> {
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let standard = GeneralPurpose::new(&base64::alphabet::STANDARD, config).decode(blob);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): text that is not base64 in either alphabet is not a blob to scan; only_json_or_pem_blobs_decode pins it"
        )
    )]
    let bytes = match standard {
        Ok(bytes) => bytes,
        Err(_) => match GeneralPurpose::new(&base64::alphabet::URL_SAFE, config).decode(blob) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        },
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): a blob that decodes to bytes that are not UTF-8 is not JSON or PEM; only_json_or_pem_blobs_decode pins it"
        )
    )]
    let Ok(text) = String::from_utf8(bytes) else {
        return None;
    };
    (text.trim_start().starts_with('{') || text.contains(PEM_OPEN)).then_some(text)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_ends_on_a_char_boundary() {
        let text = format!("{}é", "x".repeat(WINDOW - 1));
        assert!(text.is_char_boundary(window_end(&text, 0)));
    }

    #[test]
    fn only_json_or_pem_blobs_decode() {
        let json = base64::engine::general_purpose::STANDARD.encode(r#"{"a": 1}"#);
        assert!(decoded_text(&json).is_some_and(|text| text.starts_with('{')));
        let pem = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("-----BEGIN X-----");
        assert!(decoded_text(&pem).is_some());
        let noise = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xfe, 0xfd, 0x00]);
        assert!(decoded_text(&noise).is_none());
        assert!(decoded_text("not base64 at all!").is_none());
    }
}
