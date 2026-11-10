use std::ops::Range;

use std::io::Read as _;

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
use super::value::shannon_entropy;

const WINDOW: usize = 64 * 1024;
const LOOKAHEAD_LINES: usize = 16;
const DECODE_DEPTH: usize = 2;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const MESSAGE_DECODE_BYTES: u64 = 8 << 20;
const BUNDLE_DECODE_BYTES: u64 = 64 << 20;
const DER_SEQUENCE: u8 = 0x30;
const DER_INTEGER: u8 = 0x02;
const UTF8_BOM: char = '\u{feff}';

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

enum Decoded {
    Text(String),
    DerPrivateKey,
    OverBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    message: u64,
    bundle: u64,
}

impl Budget {
    #[must_use]
    pub fn after(spent: u64) -> Self {
        Self {
            message: MESSAGE_DECODE_BYTES,
            bundle: BUNDLE_DECODE_BYTES.saturating_sub(spent),
        }
    }

    #[must_use]
    pub const fn spent(&self) -> u64 {
        BUNDLE_DECODE_BYTES - self.bundle
    }

    #[must_use]
    pub const fn leaves_room(spent: u64, next: u64) -> bool {
        spent + next + MESSAGE_DECODE_BYTES <= BUNDLE_DECODE_BYTES
    }

    pub fn next_message(&mut self) {
        self.message = MESSAGE_DECODE_BYTES;
    }

    fn fits(&mut self, bytes: usize) -> bool {
        let fits = bytes as u64 <= self.left();
        if !fits {
            self.take(bytes);
        }
        fits
    }

    const fn left(&self) -> u64 {
        if self.message < self.bundle {
            self.message
        } else {
            self.bundle
        }
    }

    fn take(&mut self, bytes: usize) -> bool {
        let left = self.left();
        let bytes = bytes as u64;
        let fits = bytes <= left;
        let spent = if fits { bytes } else { left };
        self.message -= spent;
        self.bundle -= spent;
        fits
    }
}

impl Ruleset {
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span<'_>> {
        self.spans_within(text, &mut Budget::after(0))
    }

    #[must_use]
    pub fn spans_within(&self, text: &str, budget: &mut Budget) -> Vec<Span<'_>> {
        self.scan(text, 0, budget)
    }

    fn scan(&self, text: &str, depth: usize, budget: &mut Budget) -> Vec<Span<'_>> {
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
        budget: &mut Budget,
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
        budget: &mut Budget,
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

fn decoded(blob: &str, budget: &mut Budget) -> Option<Decoded> {
    let lines: Vec<&str> = blob.lines().map(str::trim).collect();
    let (&first, rest) = lines.split_first()?;
    let width = first.len();
    let mut body = vec![first];
    for line in rest {
        if line.len() == width {
            body.push(line);
        } else {
            if line.len() < width {
                body.push(line);
            }
            break;
        }
    }
    decoded_joined(&body.concat(), budget).or_else(|| {
        body.pop();
        (!body.is_empty())
            .then(|| decoded_joined(&body.concat(), budget))
            .flatten()
    })
}

fn decoded_joined(joined: &str, budget: &mut Budget) -> Option<Decoded> {
    if !budget.fits(joined.len() / 4 * 3) {
        return Some(Decoded::OverBudget);
    }
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let standard = GeneralPurpose::new(&base64::alphabet::STANDARD, config).decode(joined);
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): text that is not base64 in either alphabet is not a blob to scan; only_text_and_der_keys_decode pins it"
        )
    )]
    let bytes = match standard {
        Ok(bytes) => bytes,
        Err(_) => match GeneralPurpose::new(&base64::alphabet::URL_SAFE, config).decode(joined) {
            Ok(bytes) => bytes,
            Err(_) => return None,
        },
    };
    if !budget.take(bytes.len()) {
        return Some(Decoded::OverBudget);
    }
    let bytes = if bytes.starts_with(&GZIP_MAGIC) {
        let mut inflated = Vec::new();
        if flate2::read::GzDecoder::new(bytes.as_slice())
            .take(budget.left().saturating_add(1))
            .read_to_end(&mut inflated)
            .is_err()
        {
            return None;
        }
        if !budget.take(inflated.len()) {
            return Some(Decoded::OverBudget);
        }
        inflated
    } else {
        bytes
    };
    if is_der_private_key(&bytes) {
        return Some(Decoded::DerPrivateKey);
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B9 (note 136): a blob that decodes to bytes that are not UTF-8 is not text to scan; only_text_and_der_keys_decode pins it"
        )
    )]
    let Ok(text) = String::from_utf8(bytes) else {
        return None;
    };
    let text = text.trim_start_matches(UTF8_BOM).trim_start().to_owned();
    (!text.is_empty()).then_some(Decoded::Text(text))
}

fn is_der_private_key(bytes: &[u8]) -> bool {
    let Some((&tag, rest)) = bytes.split_first() else {
        return false;
    };
    if tag != DER_SEQUENCE {
        return false;
    }
    let Some((&length, rest)) = rest.split_first() else {
        return false;
    };
    let header = if length & 0x80 == 0 {
        0
    } else {
        usize::from(length & 0x7f)
    };
    let Some(body) = rest.get(header..) else {
        return false;
    };
    matches!(body, [DER_INTEGER, 1, 0 | 1, ..])
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

    #[test]
    fn only_text_and_der_keys_decode() {
        let json = base64::engine::general_purpose::STANDARD.encode(r#"{"a": 1}"#);
        assert!(
            matches!(decoded(&json, &mut Budget::after(0)), Some(Decoded::Text(text)) if text.starts_with('{'))
        );
        let wrapped = format!("{}\n{}", &json[..6], &json[6..]);
        assert!(matches!(
            decoded(&wrapped, &mut Budget::after(0)),
            Some(Decoded::Text(_))
        ));
        let env = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("A=b\n");
        assert!(
            matches!(decoded(&env, &mut Budget::after(0)), Some(Decoded::Text(text)) if text == "A=b\n")
        );
        let noise = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xfe, 0xfd, 0x00]);
        assert!(decoded(&noise, &mut Budget::after(0)).is_none());
        assert!(decoded("not base64 at all!", &mut Budget::after(0)).is_none());
        let pkcs8 = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x04, 0xbd, 0x02, 0x01, 0x00, 0x30]);
        assert!(matches!(
            decoded(&pkcs8, &mut Budget::after(0)),
            Some(Decoded::DerPrivateKey)
        ));
        let certificate = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x03, 0x5d, 0x30, 0x82, 0x02, 0x45]);
        assert!(decoded(&certificate, &mut Budget::after(0)).is_none());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, b"token=x").expect("write");
        let gz = base64::engine::general_purpose::STANDARD.encode(gz.finish().expect("finish"));
        assert!(
            matches!(decoded(&gz, &mut Budget::after(0)), Some(Decoded::Text(text)) if text == "token=x")
        );
    }

    fn gzip_base64(bytes: &[u8]) -> String {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut gz, bytes).expect("write");
        base64::engine::general_purpose::STANDARD.encode(gz.finish().expect("finish"))
    }

    #[test]
    fn decoding_stops_at_the_message_budget() {
        let blob = gzip_base64(&vec![b'A'; 5 << 20]);
        let mut budget = Budget::after(0);
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::Text(_))
        ));
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::OverBudget)
        ));
        budget.next_message();
        assert!(matches!(
            decoded(&blob, &mut budget),
            Some(Decoded::Text(_))
        ));
    }

    #[test]
    fn decoding_stops_at_the_bundle_budget() {
        let blob = gzip_base64(&vec![b'A'; 5 << 20]);
        let mut budget = Budget::after(0);
        let mut decoded_messages = 0;
        while matches!(decoded(&blob, &mut budget), Some(Decoded::Text(_))) {
            decoded_messages += 1;
            budget.next_message();
        }
        assert_eq!(decoded_messages, BUNDLE_DECODE_BYTES / (5 << 20));
    }
}
