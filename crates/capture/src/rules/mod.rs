mod scan;

use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use aho_corasick::MatchKind;
use regex::Regex;
use regex::RegexBuilder;
use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

pub use scan::Span;

const GITLEAKS: &str = include_str!("gitleaks.toml");
const RIVETHUB: &str = include_str!("rivethub.toml");
const RIVETHUB_FILE: &str = "rivethub.toml";
const REGEX_SIZE_LIMIT: usize = 32 << 20;

#[derive(Debug, Error)]
pub enum RuleError {
    #[error("{file} is not valid TOML: {message}")]
    Toml { file: &'static str, message: String },
    #[error("rule {id} in {file} has no regex and no path; it cannot match text")]
    NoRegex { file: &'static str, id: String },
    #[error("rule {id} in {file} has a regex that does not compile: {message}")]
    Regex {
        file: &'static str,
        id: String,
        message: String,
    },
    #[error("rule {id} in {file} names secret group {group} but its regex has {groups} groups")]
    SecretGroup {
        file: &'static str,
        id: String,
        group: usize,
        groups: usize,
    },
    #[error("rule {id} in {file} has no keywords; every rule needs a prefilter")]
    NoKeywords { file: &'static str, id: String },
    #[error("an allowlist of {id} in {file} has a regex that does not compile: {message}")]
    Allowlist {
        file: &'static str,
        id: String,
        message: String,
    },
    #[error("the secret-name regex in {file} does not compile: {message}")]
    SecretNames { file: &'static str, message: String },
    #[error("the keyword automaton does not build: {0}")]
    Keywords(#[from] aho_corasick::BuildError),
    #[error("the gitleaks file digest is {found}, but rivethub.toml pins {pinned}")]
    Pin { found: String, pinned: String },
    #[error("rivethub.toml has no [gitleaks] table pinning the vendored file")]
    Unpinned,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[expect(
        dead_code,
        reason = "parsed only so deny_unknown_fields accepts the upstream file"
    )]
    title: Option<String>,
    #[serde(rename = "minVersion")]
    #[expect(
        dead_code,
        reason = "parsed only so deny_unknown_fields accepts the upstream file"
    )]
    min_version: Option<String>,
    gitleaks: Option<Upstream>,
    secret_names: Option<SecretNames>,
    allowlist: Option<RawAllowlist>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub version: String,
    pub commit: String,
    pub file: String,
    pub sha256: String,
    pub license: String,
    pub copyright: String,
    pub license_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretNames {
    regex: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawRule {
    id: String,
    description: Option<String>,
    regex: Option<String>,
    path: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    entropy: Option<f64>,
    secret_group: Option<usize>,
    #[serde(default)]
    allowlists: Vec<RawAllowlist>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAllowlist {
    #[expect(
        dead_code,
        reason = "parsed only so deny_unknown_fields accepts the upstream file"
    )]
    description: Option<String>,
    #[serde(default)]
    regexes: Vec<String>,
    #[serde(default)]
    regex_target: Target,
    #[serde(default)]
    stopwords: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    condition: Condition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    #[default]
    Secret,
    Match,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Condition {
    #[default]
    Or,
    And,
}

#[derive(Debug)]
pub struct Allow {
    target: Target,
    regexes: Vec<Regex>,
    stopwords: Vec<String>,
    all: bool,
}

#[derive(Debug)]
pub struct Rule {
    pub id: String,
    pub description: String,
    regex: Regex,
    entropy: Option<f64>,
    secret_group: Option<usize>,
    multiline: bool,
    allow: Vec<Allow>,
}

#[derive(Debug)]
pub struct Ruleset {
    rules: Vec<Rule>,
    keywords: AhoCorasick,
    keyword_rules: Vec<Vec<usize>>,
    allow: Vec<Allow>,
    secret_names: Regex,
    upstream: Upstream,
    digest: String,
}

#[expect(
    clippy::expect_used,
    reason = "the rule files are embedded constants; every_rule_compiles_and_the_upstream_pin_holds runs load() in CI"
)]
static RULESET: LazyLock<Ruleset> =
    LazyLock::new(|| load().expect("the embedded secret rules compile"));

#[must_use]
pub fn ruleset() -> &'static Ruleset {
    &RULESET
}

pub fn load() -> Result<Ruleset, RuleError> {
    let ours = parse(RIVETHUB, "rivethub.toml")?;
    let theirs = parse(GITLEAKS, "gitleaks.toml")?;
    let upstream = ours.gitleaks.ok_or(RuleError::Unpinned)?;
    let found = hex::encode(Sha256::digest(GITLEAKS.as_bytes()));
    if found != upstream.sha256 {
        return Err(RuleError::Pin {
            found,
            pinned: upstream.sha256,
        });
    }
    let names = ours.secret_names.ok_or_else(|| RuleError::SecretNames {
        file: RIVETHUB_FILE,
        message: "missing".to_owned(),
    })?;
    let secret_names = Regex::new(&names.regex).map_err(|error| RuleError::SecretNames {
        file: RIVETHUB_FILE,
        message: error.to_string(),
    })?;
    let mut allow = Vec::new();
    for (raw, file) in [
        (ours.allowlist, "rivethub.toml"),
        (theirs.allowlist, "gitleaks.toml"),
    ] {
        if let Some(raw) = raw
            && let Some(compiled) = allowlist(raw, file, "allowlist")?
        {
            allow.push(compiled);
        }
    }
    let mut rules = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut keyword_rules: Vec<Vec<usize>> = Vec::new();
    for (raw, file) in ours
        .rules
        .into_iter()
        .map(|raw| (raw, "rivethub.toml"))
        .chain(theirs.rules.into_iter().map(|raw| (raw, "gitleaks.toml")))
    {
        if raw.path.is_some() {
            continue;
        }
        let index = rules.len();
        for keyword in &raw.keywords {
            let keyword = keyword.to_lowercase();
            if let Some(at) = patterns.iter().position(|known| *known == keyword) {
                if let Some(list) = keyword_rules.get_mut(at) {
                    list.push(index);
                }
            } else {
                patterns.push(keyword);
                keyword_rules.push(vec![index]);
            }
        }
        rules.push(rule(raw, file)?);
    }
    let keywords = AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::Standard)
        .build(&patterns)?;
    let mut hasher = Sha256::new();
    hasher.update(RIVETHUB.as_bytes());
    hasher.update([0]);
    hasher.update(GITLEAKS.as_bytes());
    Ok(Ruleset {
        rules,
        keywords,
        keyword_rules,
        allow,
        secret_names,
        upstream,
        digest: hex::encode(hasher.finalize()),
    })
}

fn parse(text: &str, file: &'static str) -> Result<File, RuleError> {
    toml::from_str(text).map_err(|error| RuleError::Toml {
        file,
        message: error.message().to_owned(),
    })
}

fn rule(raw: RawRule, file: &'static str) -> Result<Rule, RuleError> {
    let id = raw.id;
    let source = raw.regex.ok_or_else(|| RuleError::NoRegex {
        file,
        id: id.clone(),
    })?;
    let regex = compile(&source).map_err(|error| RuleError::Regex {
        file,
        id: id.clone(),
        message: error.to_string(),
    })?;
    if let Some(group) = raw.secret_group
        && group >= regex.captures_len()
    {
        return Err(RuleError::SecretGroup {
            file,
            id,
            group,
            groups: regex.captures_len() - 1,
        });
    }
    if raw.keywords.is_empty() {
        return Err(RuleError::NoKeywords { file, id });
    }
    let mut allow = Vec::new();
    for raw in raw.allowlists {
        if let Some(compiled) = allowlist(raw, file, &id)? {
            allow.push(compiled);
        }
    }
    Ok(Rule {
        description: raw.description.unwrap_or_default(),
        multiline: source.contains("\\n") || source.contains("\\r") || source.contains("\\s\\S"),
        regex,
        entropy: raw.entropy,
        secret_group: raw.secret_group,
        allow,
        id,
    })
}

fn allowlist(raw: RawAllowlist, file: &'static str, id: &str) -> Result<Option<Allow>, RuleError> {
    let all = raw.condition == Condition::And;
    if all && !raw.paths.is_empty() {
        return Ok(None);
    }
    if raw.regexes.is_empty() && raw.stopwords.is_empty() {
        return Ok(None);
    }
    let regexes = raw
        .regexes
        .into_iter()
        .map(|source| compile(&source))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RuleError::Allowlist {
            file,
            id: id.to_owned(),
            message: error.to_string(),
        })?;
    Ok(Some(Allow {
        target: raw.regex_target,
        regexes,
        stopwords: raw
            .stopwords
            .into_iter()
            .map(|word| word.to_lowercase())
            .collect(),
        all,
    }))
}

fn compile(source: &str) -> Result<Regex, regex::Error> {
    let source = rust_syntax(source);
    RegexBuilder::new(&source)
        .unicode(false)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .or_else(|_| {
            RegexBuilder::new(&source)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
        })
}

fn rust_syntax(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len() + 8);
    let mut in_class = false;
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        match c {
            '\\' => {
                out.push(c);
                if let Some(&next) = chars.get(index + 1) {
                    out.push(next);
                    index += 1;
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push(c);
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            '{' if !in_class && !repetition_follows(&chars, index) => out.push_str("\\{"),
            '}' if !in_class && !repetition_closes(&chars, index) => out.push_str("\\}"),
            _ => out.push(c),
        }
        index += 1;
    }
    out
}

fn repetition_follows(chars: &[char], open: usize) -> bool {
    let mut index = open + 1;
    let mut digits = 0;
    while chars.get(index).is_some_and(char::is_ascii_digit) {
        index += 1;
        digits += 1;
    }
    if digits == 0 {
        return false;
    }
    if chars.get(index) == Some(&',') {
        index += 1;
        while chars.get(index).is_some_and(char::is_ascii_digit) {
            index += 1;
        }
    }
    chars.get(index) == Some(&'}')
}

fn repetition_closes(chars: &[char], close: usize) -> bool {
    let mut index = close;
    while index > 0 {
        index -= 1;
        match chars.get(index) {
            Some(c) if c.is_ascii_digit() || *c == ',' => {}
            Some('{') => return repetition_follows(chars, index),
            _ => return false,
        }
    }
    false
}

impl Ruleset {
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    #[must_use]
    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn is_secret_name(&self, key: &str) -> bool {
        self.secret_names
            .find(key)
            .is_some_and(|found| !found.range().is_empty())
    }

    #[must_use]
    pub fn emits(&self, kind: &str) -> bool {
        self.rules.iter().any(|rule| rule.id == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_compiles_and_the_upstream_pin_holds() {
        let rules = load().unwrap_or_else(|error| panic!("{error}"));
        assert!(rules.rules().len() > 200, "{} rules", rules.rules().len());
        assert_eq!(rules.upstream().version, "v8.30.1");
        assert!(rules.emits("github_token"));
        assert!(rules.emits("github-pat"));
        assert!(!rules.emits("ghp_abcdefghij"));
    }

    #[test]
    fn re2_literal_braces_become_rust_escapes() {
        assert_eq!(rust_syntax(r"^\$(?:\d+|{\d+})$"), r"^\$(?:\d+|\{\d+\})$");
        assert_eq!(rust_syntax(r"{{[^}]+}}"), r"\{\{[^}]+\}\}");
        assert_eq!(rust_syntax(r"a{2,3}b{4}c{5,}\{d"), r"a{2,3}b{4}c{5,}\{d");
    }
}
