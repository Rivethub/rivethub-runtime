mod pairs;
mod spaced;

use std::collections::HashSet;
use std::ops::Range;

use regex::Regex;
use serde::Deserialize;

use super::Allow;
use super::RuleError;
use super::compile::compile;
use super::value;
use super::value::Shape;
use super::value::Value;
use super::value::normalize;
use super::value::shannon_entropy;

const RIVETHUB_FILE: &str = "rivethub.toml";
const PLACEHOLDER_OPEN: &str = "[REDACTED";
const PATTERN_KIND: &str = "pattern:";
const SECRET_KEY_KIND: &str = "secret_key";
pub(crate) const FLAGGED_KIND: &str = "flagged";
pub(crate) const FLAGGED_OPEN: &str = "[FLAGGED:";
const KEY_WORD: &str = "key";
const MIN_ECHO_LENGTH: usize = 4;
const MAX_CODE_SEGMENT: usize = 40;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNames {
    password: Vec<String>,
    token: Vec<String>,
    key_never: Vec<String>,
    exact_key: Vec<String>,
    suffix_only: Vec<String>,
    exact_password: Vec<String>,
    exact_token: Vec<String>,
    value_suffixes: Vec<String>,
    vendor_prefixes: Vec<String>,
    stopwords: Vec<String>,
    templates: Vec<String>,
    separators: Vec<String>,
    comment_openers: Vec<String>,
    code_values: Vec<String>,
    min_length: usize,
    min_entropy: f64,
    key_min_length: usize,
    key_min_entropy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Password,
    Token,
    Key,
}

#[derive(Debug, Clone, Copy, Default)]
struct Context<'a> {
    key: &'a str,
    key_quoted: bool,
    in_comment: bool,
    prose_follows: bool,
}

enum Verdict {
    Keep,
    Redact,
    Inner(Vec<Range<usize>>),
}

#[derive(Debug)]
pub struct Names {
    password: Vec<String>,
    token: Vec<String>,
    key_never: Vec<String>,
    exact_key: Vec<String>,
    suffix_only: Vec<String>,
    exact_password: Vec<String>,
    exact_token: Vec<String>,
    value_suffixes: Vec<String>,
    vendor_prefixes: Vec<String>,
    stopwords: HashSet<String>,
    templates: Vec<Regex>,
    code: Vec<Regex>,
    comment_openers: Vec<String>,
    kinds: HashSet<String>,
    min_length: usize,
    min_entropy: f64,
    key_min_length: usize,
    key_min_entropy: f64,
    forms: Forms,
}

#[derive(Debug)]
struct Forms {
    exact_stopwords: Option<Regex>,
    candidate: Regex,
    spaced: Regex,
    number: Regex,
    junk: Regex,
    version: Regex,
    identifier: Regex,
    word: Regex,
    line_prefix: Regex,
    embedded_template: Regex,
}

impl Forms {
    fn build(raw: &RawNames) -> Result<Self, RuleError> {
        let fixed = |source: &str| {
            Regex::new(source).map_err(|error| RuleError::SecretNames {
                file: RIVETHUB_FILE,
                message: error.to_string(),
            })
        };
        let escaped = |words: &[String]| {
            words
                .iter()
                .map(|word| regex::escape(word))
                .collect::<Vec<_>>()
                .join("|")
        };
        let exact_stopwords = (!raw.stopwords.is_empty())
            .then(|| fixed(&format!("^(?i:{})$", escaped(&raw.stopwords))))
            .transpose()?;
        let mut separators = raw.separators.clone();
        separators.sort_by_key(|separator| std::cmp::Reverse(separator.len()));
        let mut spaced_words: Vec<String> = Vec::new();
        for word in raw.password.iter().chain(&raw.token) {
            if raw.suffix_only.contains(word) {
                continue;
            }
            for variant in [word.clone(), word.replace('_', "-"), word.replace('_', "")] {
                if !spaced_words.contains(&variant) {
                    spaced_words.push(variant);
                }
            }
        }
        Ok(Self {
            exact_stopwords,
            candidate: fixed(&format!(
                r"(?i)\b([A-Za-z_][A-Za-z0-9_.-]*)(\\?[\x22'])?[ \t]*(?:{})[ \t]*",
                escaped(&separators)
            ))?,
            spaced: fixed(&format!(
                r"(?i)(?:^|[\s(,])({})[ \t]+",
                escaped(&spaced_words)
            ))?,
            number: fixed(r"^-?\d+(?:\.\d+)?$")?,
            junk: fixed(r"^(?:\\[nrt0]|\s)*$")?,
            version: fixed(r"^[\^~=<>v]*\d+(?:\.\d+)+(?:[-_+][\w.]+)?(?:\([^()]*\))*$")?,
            identifier: fixed(r"^[A-Za-z_]\w*$")?,
            word: fixed(r"^[A-Za-z]+$")?,
            line_prefix: fixed(r"^(?:[+\->|]\s*)?\d*[\s\t]*")?,
            embedded_template: fixed(
                r"\$\{\{[^}]*\}\}|\$\{[A-Za-z_][\w.:-]*\}|\{\{[^}]*\}\}|%[A-Z_][A-Z0-9_]*%",
            )?,
        })
    }
}

impl Names {
    pub(super) fn build(raw: RawNames) -> Result<Self, RuleError> {
        let list = |sources: &[String], id: &str| {
            sources
                .iter()
                .map(|source| compile(source))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| RuleError::Allowlist {
                    file: RIVETHUB_FILE,
                    id: id.to_owned(),
                    message: error.to_string(),
                })
        };
        let templates = list(&raw.templates, "names.templates")?;
        let code = list(&raw.code_values, "names.code_values")?;
        let forms = Forms::build(&raw)?;
        let normalized = |names: &[String]| names.iter().map(|name| normalize(name)).collect();
        Ok(Self {
            password: normalized(&raw.password),
            token: normalized(&raw.token),
            key_never: normalized(&raw.key_never),
            exact_key: normalized(&raw.exact_key),
            suffix_only: normalized(&raw.suffix_only),
            exact_password: normalized(&raw.exact_password),
            exact_token: normalized(&raw.exact_token),
            value_suffixes: normalized(&raw.value_suffixes),
            vendor_prefixes: raw.vendor_prefixes,
            stopwords: raw
                .stopwords
                .into_iter()
                .map(|word| word.to_lowercase())
                .collect(),
            templates,
            code,
            comment_openers: raw.comment_openers,
            kinds: HashSet::new(),
            min_length: raw.min_length,
            min_entropy: raw.min_entropy,
            key_min_length: raw.key_min_length,
            key_min_entropy: raw.key_min_entropy,
            forms,
        })
    }

    pub(super) fn set_kinds(&mut self, kinds: impl IntoIterator<Item = String>) {
        self.kinds = kinds.into_iter().collect();
        self.kinds.insert(SECRET_KEY_KIND.to_owned());
        self.kinds.insert(FLAGGED_KIND.to_owned());
    }

    pub(super) fn allow(&self) -> Allow {
        let mut regexes = self.templates.clone();
        regexes.extend(self.forms.exact_stopwords.clone());
        Allow::exact(regexes)
    }

    pub(super) fn keywords(&self) -> Vec<String> {
        let mut words = Vec::new();
        for name in self
            .password
            .iter()
            .chain(&self.token)
            .chain(&self.exact_password)
            .chain(&self.exact_token)
            .chain(&self.exact_key)
        {
            for variant in [name.clone(), name.replace('_', ""), name.replace('_', "-")] {
                if !words.contains(&variant) {
                    words.push(variant);
                }
            }
        }
        words.extend(["_key", "-key", "key=", "key:", "key ", "key\""].map(str::to_owned));
        words.retain(|word| word != KEY_WORD);
        words
    }

    #[must_use]
    pub fn family(&self, key: &str) -> Option<Family> {
        if self.is_placeholder(key) {
            return None;
        }
        let name = normalize(key);
        if name.is_empty() {
            return None;
        }
        self.family_of(&name).or_else(|| {
            let (head, last) = name.rsplit_once('_')?;
            self.value_suffixes
                .contains(&last.to_owned())
                .then(|| self.family_of(head))
                .flatten()
        })
    }

    fn family_of(&self, name: &str) -> Option<Family> {
        let singular = name.strip_suffix('s').filter(|rest| !rest.is_empty());
        self.family_exact(name)
            .or_else(|| singular.and_then(|rest| self.family_exact(rest)))
    }

    fn family_exact(&self, name: &str) -> Option<Family> {
        let owned = name.to_owned();
        if self.exact_password.contains(&owned) {
            return Some(Family::Password);
        }
        if self.exact_token.contains(&owned) {
            return Some(Family::Token);
        }
        if self.exact_key.contains(&owned) {
            return Some(Family::Key);
        }
        let ends_with = |family: &[String]| {
            family.iter().any(|word| {
                name.ends_with(&format!("_{word}"))
                    || (name == *word && !self.suffix_only.contains(word))
            })
        };
        if ends_with(&self.password) {
            return Some(Family::Password);
        }
        if ends_with(&self.token) {
            return Some(Family::Token);
        }
        let key_like = name.ends_with(&format!("_{KEY_WORD}"));
        (key_like && !self.key_never.contains(&owned)).then_some(Family::Key)
    }

    #[must_use]
    pub fn redacts(&self, key: &str, value: &str) -> bool {
        self.family(key).is_some_and(|family| {
            matches!(
                self.judge(family, value, Shape::Quoted, Context::default()),
                Verdict::Redact
            )
        })
    }

    fn judge(&self, family: Family, value: &str, shape: Shape, ctx: Context<'_>) -> Verdict {
        let value = value.get(self.placeholder_len(value)..).unwrap_or_default();
        if self.trivially_kept(value) {
            return Verdict::Keep;
        }
        let (value, shape) = if shape == Shape::Bracketed {
            if let Some(strings) = value::inner_strings(value) {
                return Verdict::Inner(
                    strings
                        .into_iter()
                        .filter(|range| {
                            value.get(range.clone()).is_some_and(|inner| {
                                (!self.forms.word.is_match(inner) || inner.len() >= self.min_length)
                                    && matches!(
                                        self.judge(family, inner, Shape::Quoted, ctx),
                                        Verdict::Redact
                                    )
                            })
                        })
                        .collect(),
                );
            }
            (value::unwrap_brackets(value), Shape::Bare)
        } else {
            (value, shape)
        };
        if shape == Shape::Bare && self.trivially_kept(value) {
            return Verdict::Keep;
        }
        if self.forms.number.is_match(value) {
            let json_number = shape == Shape::Bare && ctx.key_quoted;
            return if family == Family::Password && !json_number {
                Verdict::Redact
            } else {
                Verdict::Keep
            };
        }
        let vendor = self
            .vendor_prefixes
            .iter()
            .any(|prefix| value.starts_with(prefix.as_str()));
        if shape == Shape::Bare
            && !vendor
            && (self.looks_like_code(value)
                || self.echoes_key(value, ctx.key)
                || ((family != Family::Password || ctx.key_quoted)
                    && self.forms.version.is_match(value))
                || (ctx.in_comment && ctx.prose_follows && self.forms.word.is_match(value)))
        {
            return Verdict::Keep;
        }
        let (length, entropy) = match family {
            Family::Password => return Verdict::Redact,
            Family::Token => {
                if value.contains(char::is_whitespace) {
                    return Verdict::Keep;
                }
                (self.min_length, self.min_entropy)
            }
            Family::Key => {
                if !value.chars().any(|c| c.is_ascii_digit()) || value.contains(char::is_whitespace)
                {
                    return Verdict::Keep;
                }
                (self.key_min_length, self.key_min_entropy)
            }
        };
        if vendor || (value.chars().count() >= length && shannon_entropy(value) >= entropy) {
            Verdict::Redact
        } else {
            Verdict::Keep
        }
    }

    fn trivially_kept(&self, value: &str) -> bool {
        value.is_empty()
            || value.starts_with(['=', ':', '>'])
            || !value.chars().any(char::is_alphanumeric)
            || self.forms.junk.is_match(value)
            || self.is_placeholder(value)
            || self.stopwords.contains(&value.to_lowercase())
            || self
                .templates
                .iter()
                .any(|template| template.is_match(value))
            || self.forms.embedded_template.is_match(value)
    }

    fn looks_like_code(&self, value: &str) -> bool {
        let ternary = value
            .split_once(':')
            .is_some_and(|(head, _)| self.code.first().is_some_and(|path| path.is_match(head)));
        ternary
            || self.code.iter().any(|code| code.is_match(value))
                && (value.contains(['(', '['])
                    || value
                        .split(['.', ':'])
                        .all(|segment| segment.chars().count() <= MAX_CODE_SEGMENT))
    }

    fn echoes_key(&self, value: &str, key: &str) -> bool {
        if value.len() < MIN_ECHO_LENGTH || !self.forms.identifier.is_match(value) {
            return false;
        }
        let key = normalize(key);
        let value = normalize(value);
        let last = key.rsplit('_').next().unwrap_or(&key);
        key == value
            || key.ends_with(&format!("_{value}"))
            || (value.contains('_') && value.ends_with(&format!("_{last}")))
    }

    pub(super) fn hits(&self, text: &str) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        let mut at = 0;
        let mut lines = value::Lines::default();
        let mut kept = value::KeptBudget::for_text(text);
        while let Some(captures) = self.forms.candidate.captures_at(text, at) {
            let Some(whole) = captures.get(0) else {
                break;
            };
            at = whole.end();
            let Some(key) = captures
                .get(1)
                .filter(|key| !value::expansion_or_url_user(text, key.start()))
            else {
                continue;
            };
            let Some(family) = self.family(key.as_str()) else {
                continue;
            };
            let Some(rest) = text.get(whole.end()..) else {
                break;
            };
            let line_end = lines.end(text, whole.end()) - whole.end();
            let Some(value) = value::parse(rest, line_end, &|text| self.placeholder_len(text))
            else {
                continue;
            };
            let value = self.past_type_annotation(rest, value, |at| {
                lines.end(text, whole.end() + at) - whole.end() - at
            });
            let ctx = Context {
                key: key.as_str(),
                key_quoted: captures.get(2).is_some(),
                in_comment: value::in_comment(
                    text,
                    lines.start(text, key.start()),
                    key.start(),
                    &self.forms.line_prefix,
                    &self.comment_openers,
                ),
                prose_follows: value::prose_follows(rest, value.range.end),
            };
            let base = whole.end();
            let Some(slice) = rest.get(value.range.clone()) else {
                continue;
            };
            let verdict = if kept.exhausted() {
                Verdict::Redact
            } else {
                self.judge(family, slice, value.shape, ctx)
            };
            match verdict {
                Verdict::Keep => kept.spend(value.range.end),
                Verdict::Redact => {
                    found.push(base + value.range.start..base + value.range.end);
                    at = base + value.range.end;
                }
                Verdict::Inner(ranges) => {
                    let offset = base + value.range.start;
                    found.extend(
                        ranges
                            .into_iter()
                            .map(|range| offset + range.start..offset + range.end),
                    );
                    at = base + value.range.end;
                }
            }
        }
        let spaced = self.spaced_hits(text, &found);
        found.extend(spaced);
        found.sort_by_key(|range| range.start);
        found.dedup_by(|next, kept| next.start < kept.end);
        found
    }

    fn past_type_annotation(
        &self,
        rest: &str,
        value: Value,
        mut line_end: impl FnMut(usize) -> usize,
    ) -> Value {
        if value.shape != Shape::Bare {
            return value;
        }
        let is_type = rest
            .get(value.range.clone())
            .is_some_and(|slice| self.code.iter().any(|code| code.is_match(slice)));
        let tail = rest.get(value.range.end..).unwrap_or_default();
        let trimmed = tail.trim_start_matches([' ', '\t']);
        let assigns =
            trimmed.starts_with('=') && !trimmed.starts_with("==") && !trimmed.starts_with("=>");
        if !is_type || !assigns {
            return value;
        }
        let after = rest.len() - trimmed.len() + 1;
        let spaces = trimmed.get(1..).map_or(0, |body| {
            body.len() - body.trim_start_matches([' ', '\t']).len()
        });
        let start = after + spaces;
        match rest.get(start..).and_then(|tail| {
            value::parse(tail, line_end(start), &|text| self.placeholder_len(text))
        }) {
            Some(inner) => Value {
                range: start + inner.range.start..start + inner.range.end,
                shape: inner.shape,
            },
            None => value,
        }
    }

    pub(crate) fn placeholder_len(&self, value: &str) -> usize {
        if let Some(rest) = value.strip_prefix(FLAGGED_OPEN) {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            return if digits > 0 && rest.as_bytes().get(digits) == Some(&b']') {
                FLAGGED_OPEN.len() + digits + 1
            } else {
                0
            };
        }
        let Some(rest) = value.strip_prefix(PLACEHOLDER_OPEN) else {
            return 0;
        };
        if rest.starts_with(']') {
            return PLACEHOLDER_OPEN.len() + 1;
        }
        let Some(kind) = rest.strip_prefix(':') else {
            return 0;
        };
        let Some(close) = kind.find(']') else {
            return 0;
        };
        let name = kind.get(..close).unwrap_or_default();
        if self.known_kind(name) {
            PLACEHOLDER_OPEN.len() + 1 + close + 1
        } else {
            0
        }
    }

    fn known_kind(&self, kind: &str) -> bool {
        self.kinds.contains(kind)
            || kind
                .strip_prefix(PATTERN_KIND)
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
    }

    #[must_use]
    pub fn is_placeholder(&self, key: &str) -> bool {
        let body = key.rsplit_once('#').map_or(key, |(body, suffix)| {
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
                body
            } else {
                key
            }
        });
        let length = self.placeholder_len(body);
        length > 0 && length == body.len()
    }
}
