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
const INFLATE_LIMIT: u64 = 4 << 20;
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
}

impl Ruleset {
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span<'_>> {
        self.scan(text, 0)
    }

    fn scan(&self, text: &str, depth: usize) -> Vec<Span<'_>> {
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
                } else if matches!(rule.matcher, Matcher::Names) && continues(text, start, line) {
                    lookahead_end(text, line)
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
                self.matches(rule, slice, scope, &mut found);
            }
        }
        for &index in &self.always {
            if let Some(rule) = self.rules.get(index) {
                let scope = Scope {
                    order: index,
                    offset: 0,
                    depth,
                };
                self.matches(rule, text, scope, &mut found);
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

    fn matches<'a>(&'a self, rule: &'a Rule, slice: &str, scope: Scope, out: &mut Vec<Span<'a>>) {
        let Scope {
            order,
            offset,
            depth,
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
                let hidden = depth < DECODE_DEPTH
                    && match decoded(secret.as_str()) {
                        Some(Decoded::DerPrivateKey) => true,
                        Some(Decoded::Text(inner)) => !self.scan(&inner, depth + 1).is_empty(),
                        None => false,
                    };
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

fn decoded(blob: &str) -> Option<Decoded> {
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
    decoded_joined(&body.concat()).or_else(|| {
        body.pop();
        (!body.is_empty())
            .then(|| decoded_joined(&body.concat()))
            .flatten()
    })
}

fn decoded_joined(joined: &str) -> Option<Decoded> {
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
    let bytes = if bytes.starts_with(&GZIP_MAGIC) {
        let mut inflated = Vec::new();
        if flate2::read::GzDecoder::new(bytes.as_slice())
            .take(INFLATE_LIMIT)
            .read_to_end(&mut inflated)
            .is_err()
        {
            return None;
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

fn continues(text: &str, start: usize, line: usize) -> bool {
    let tail = text.get(start..line).unwrap_or_default().trim_end();
    tail.ends_with(['=', ':', '|', '>']) || tail.ends_with("|-") || tail.ends_with(">-")
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
        assert!(matches!(decoded(&json), Some(Decoded::Text(text)) if text.starts_with('{')));
        let wrapped = format!("{}\n{}", &json[..6], &json[6..]);
        assert!(matches!(decoded(&wrapped), Some(Decoded::Text(_))));
        let env = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("A=b\n");
        assert!(matches!(decoded(&env), Some(Decoded::Text(text)) if text == "A=b\n"));
        let noise = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xfe, 0xfd, 0x00]);
        assert!(decoded(&noise).is_none());
        assert!(decoded("not base64 at all!").is_none());
        let pkcs8 = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x04, 0xbd, 0x02, 0x01, 0x00, 0x30]);
        assert!(matches!(decoded(&pkcs8), Some(Decoded::DerPrivateKey)));
        let certificate = base64::engine::general_purpose::STANDARD
            .encode([0x30, 0x82, 0x03, 0x5d, 0x30, 0x82, 0x02, 0x45]);
        assert!(decoded(&certificate).is_none());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, b"token=x").expect("write");
        let gz = base64::engine::general_purpose::STANDARD.encode(gz.finish().expect("finish"));
        assert!(matches!(decoded(&gz), Some(Decoded::Text(text)) if text == "token=x"));
    }
}
