use std::collections::HashSet;
use std::ops::Range;

use regex::Regex;
use serde::Deserialize;

use super::Allow;
use super::RuleError;
use super::compile::compile;

const RIVETHUB_FILE: &str = "rivethub.toml";
const PLACEHOLDER_OPEN: &str = "[REDACTED";
const UNQUOTED_END: &[char] = &[',', ';', '&', ')', ']', '}', '"', '\''];
const BRACKETS: &[(&str, &str)] = &[
    ("${", "}"),
    ("$(", ")"),
    ("{{", "}}"),
    ("{", "}"),
    ("<", ">"),
    ("%", "%"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawNames {
    password: Vec<String>,
    token: Vec<String>,
    suffix_only: Vec<String>,
    exact_password: Vec<String>,
    exact_token: Vec<String>,
    vendor_prefixes: Vec<String>,
    stopwords: Vec<String>,
    templates: Vec<String>,
    min_length: usize,
    min_entropy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Password,
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quoting {
    Quoted,
    Bare,
}

#[derive(Debug)]
pub struct Names {
    password: Vec<String>,
    token: Vec<String>,
    suffix_only: Vec<String>,
    exact_password: Vec<String>,
    exact_token: Vec<String>,
    vendor_prefixes: Vec<String>,
    stopwords: HashSet<String>,
    exact_stopwords: Option<Regex>,
    templates: Vec<Regex>,
    min_length: usize,
    min_entropy: f64,
    candidate: Regex,
    number: Regex,
    junk: Regex,
    version: Regex,
}

impl Names {
    pub(super) fn build(raw: RawNames) -> Result<Self, RuleError> {
        let templates = raw
            .templates
            .iter()
            .map(|source| compile(source))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RuleError::Allowlist {
                file: RIVETHUB_FILE,
                id: "names".to_owned(),
                message: error.to_string(),
            })?;
        let fixed = |source: &str| {
            Regex::new(source).map_err(|error| RuleError::SecretNames {
                file: RIVETHUB_FILE,
                message: error.to_string(),
            })
        };
        let exact_stopwords = (!raw.stopwords.is_empty())
            .then(|| {
                let words: Vec<String> = raw
                    .stopwords
                    .iter()
                    .map(|word| regex::escape(word))
                    .collect();
                fixed(&format!("^(?i:{})$", words.join("|")))
            })
            .transpose()?;
        Ok(Self {
            password: raw.password.iter().map(|name| normalize(name)).collect(),
            token: raw.token.iter().map(|name| normalize(name)).collect(),
            suffix_only: raw.suffix_only.iter().map(|name| normalize(name)).collect(),
            exact_password: raw
                .exact_password
                .iter()
                .map(|name| normalize(name))
                .collect(),
            exact_token: raw.exact_token.iter().map(|name| normalize(name)).collect(),
            vendor_prefixes: raw.vendor_prefixes,
            stopwords: raw
                .stopwords
                .into_iter()
                .map(|word| word.to_lowercase())
                .collect(),
            exact_stopwords,
            templates,
            min_length: raw.min_length,
            min_entropy: raw.min_entropy,
            candidate: fixed(r"(?i)\b([A-Za-z_][A-Za-z0-9_.-]*)(?:\\?[\x22'])?[ \t]*[=:][ \t]*")?,
            number: fixed(r"^-?\d+(?:\.\d+)?$")?,
            junk: fixed(r"^(?:\\[nrt0]|\s)*$")?,
            version: fixed(r"^[\^~=<>v]*\d+(?:\.\d+)+(?:-[\w.]+)?$")?,
        })
    }

    pub(super) fn allow(&self) -> Allow {
        let mut regexes = self.templates.clone();
        regexes.extend(self.exact_stopwords.clone());
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
        {
            for variant in [name.clone(), name.replace('_', ""), name.replace('_', "-")] {
                if !words.contains(&variant) {
                    words.push(variant);
                }
            }
        }
        words
    }

    #[must_use]
    pub fn family(&self, key: &str) -> Option<Family> {
        if is_placeholder(key) {
            return None;
        }
        let name = normalize(key);
        if name.is_empty() {
            return None;
        }
        if self.exact_password.contains(&name) {
            return Some(Family::Password);
        }
        if self.exact_token.contains(&name) {
            return Some(Family::Token);
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
        None
    }

    #[must_use]
    pub fn redacts(&self, key: &str, value: &str) -> bool {
        self.family(key)
            .is_some_and(|family| self.is_secret(family, value, Quoting::Quoted))
    }

    fn is_secret(&self, family: Family, value: &str, quoting: Quoting) -> bool {
        if value.is_empty()
            || value.starts_with(['=', ':', '>'])
            || !value.chars().any(char::is_alphanumeric)
            || self.junk.is_match(value)
        {
            return false;
        }
        let lowered = value.to_lowercase();
        if self.stopwords.contains(&lowered)
            || self.number.is_match(value)
            || self
                .templates
                .iter()
                .any(|template| template.is_match(value))
        {
            return false;
        }
        if quoting == Quoting::Bare
            && (value.starts_with(['{', '[', '('])
                || value.contains('(')
                || value.contains("::")
                || self.version.is_match(value))
        {
            return false;
        }
        match family {
            Family::Password => true,
            Family::Token => {
                self.vendor_prefixes
                    .iter()
                    .any(|prefix| value.starts_with(prefix.as_str()))
                    || (value.chars().count() >= self.min_length
                        && shannon_entropy(value) >= self.min_entropy)
            }
        }
    }

    pub(super) fn hits(&self, text: &str) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(captures) = self.candidate.captures_at(text, at) {
            let Some(whole) = captures.get(0) else {
                break;
            };
            at = whole.end();
            let Some(key) = captures.get(1) else {
                continue;
            };
            let Some(family) = self.family(key.as_str()) else {
                continue;
            };
            let Some(rest) = text.get(whole.end()..) else {
                break;
            };
            let Some((range, quoting)) = value_at(rest) else {
                continue;
            };
            let Some(value) = rest.get(range.clone()) else {
                continue;
            };
            let prefix = placeholder_len(value);
            let Some(value) = value.get(prefix..) else {
                continue;
            };
            let start = range.start + prefix;
            if self.is_secret(family, value, quoting) {
                found.push(whole.end() + start..whole.end() + range.end);
                at = whole.end() + range.end;
            }
        }
        found
    }
}

fn value_at(rest: &str) -> Option<(Range<usize>, Quoting)> {
    let line_end = rest.find(['\n', '\r']).unwrap_or(rest.len());
    let line = rest.get(..line_end)?;
    for opener in ["\\\"", "\\'", "\"", "'"] {
        if let Some(inner) = line.strip_prefix(opener) {
            let start = opener.len();
            let end = closing_quote(inner, opener).map_or(line_end, |close| start + close);
            return (end > start).then_some((start..end, Quoting::Quoted));
        }
    }
    if let Some((_, close)) = BRACKETS.iter().find(|(open, _)| line.starts_with(open))
        && let Some(after) = line.get(1..).and_then(|rest| rest.find(close))
    {
        return Some((0..1 + after + close.len(), Quoting::Bare));
    }
    let skipped = placeholder_len(line);
    let tail = line.get(skipped..)?;
    let end = skipped
        + tail
            .find(|c: char| c.is_whitespace() || UNQUOTED_END.contains(&c))
            .unwrap_or(tail.len());
    (end > 0).then_some((0..end, Quoting::Bare))
}

fn closing_quote(inner: &str, opener: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = inner.get(from..)?.find(opener) {
        let at = from + found;
        let escaped = opener.len() == 1 && inner.get(..at).is_some_and(|head| head.ends_with('\\'));
        if !escaped {
            return Some(at);
        }
        from = at + opener.len();
    }
    None
}

fn placeholder_len(value: &str) -> usize {
    let Some(rest) = value.strip_prefix(PLACEHOLDER_OPEN) else {
        return 0;
    };
    match rest.find(']') {
        Some(close) if close == 0 || rest.starts_with(':') => PLACEHOLDER_OPEN.len() + close + 1,
        Some(_) | None => 0,
    }
}

#[must_use]
pub fn is_placeholder(key: &str) -> bool {
    let Some(rest) = key.strip_prefix(PLACEHOLDER_OPEN) else {
        return false;
    };
    let body = rest.rsplit_once('#').map_or(rest, |(body, suffix)| {
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            body
        } else {
            rest
        }
    });
    body == "]" || (body.starts_with(':') && body.ends_with(']') && !body.contains('['))
}

fn normalize(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let chars: Vec<char> = key.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        let previous = index.checked_sub(1).and_then(|at| chars.get(at));
        let next = chars.get(index + 1);
        let boundary = c.is_uppercase()
            && previous.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            });
        if boundary && !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out.trim_matches('_').to_owned()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a secret is far shorter than 2^52 bytes, so the counts convert exactly"
)]
#[must_use]
pub fn shannon_entropy(text: &str) -> f64 {
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
    fn keys_normalize_to_snake_case_segments() {
        assert_eq!(normalize("X-Api-Key"), "x_api_key");
        assert_eq!(normalize("clientSecret"), "client_secret");
        assert_eq!(normalize("APIKey"), "api_key");
        assert_eq!(normalize("_authToken"), "auth_token");
        assert_eq!(normalize("secretName"), "secret_name");
        assert_eq!(normalize("x.auth.y"), "x_auth_y");
        assert_eq!(normalize("PGPASSWORD"), "pgpassword");
    }

    #[test]
    fn placeholders_are_recognized_with_or_without_a_kind_or_suffix() {
        assert!(is_placeholder("[REDACTED]"));
        assert!(is_placeholder("[REDACTED]#2"));
        assert!(is_placeholder("[REDACTED:github_token]"));
        assert!(is_placeholder("[REDACTED:pattern:3]#12"));
        assert!(!is_placeholder("[REDACTED:ghp_abc] tail"));
        assert!(!is_placeholder("token"));
    }

    #[test]
    fn entropy_grows_with_variety() {
        assert!(shannon_entropy("aaaaaaaa") < 0.01);
        assert!(shannon_entropy("abcdefgh") > 2.9);
        assert!(shannon_entropy("Zq8RmT2vLx9WcN4bKp7HyJ3dFs6GtA1e") > 4.5);
    }
}
