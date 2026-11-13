mod compile;
mod decode;
pub mod injection;
pub mod names;
mod scan;
mod value;

use std::sync::LazyLock;

use aho_corasick::AhoCorasick;
use aho_corasick::MatchKind;
use regex::Regex;
use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

use compile::compile;
use compile::compile_allow;
pub use decode::Budget;
pub use decode::Pool;
pub use decode::Spent;
pub use names::Family;
pub use names::Names;
pub use scan::Span;

const GITLEAKS: &str = include_str!("gitleaks.toml");
const RIVETHUB: &str = include_str!("rivethub.toml");
const RIVETHUB_FILE: &str = "rivethub.toml";
const GITLEAKS_FILE: &str = "gitleaks.toml";
const ALWAYS: &str = "*";

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
    #[error("rule {id} in {file} has no family or no why; every injection rule names both")]
    NoFamily { file: &'static str, id: String },
    #[error("an allowlist of {id} in {file} has a regex that does not compile: {message}")]
    Allowlist {
        file: &'static str,
        id: String,
        message: String,
    },
    #[error("the [names] table in {file} does not load: {message}")]
    SecretNames { file: &'static str, message: String },
    #[error("the keyword automaton does not build: {0}")]
    Keywords(#[from] aho_corasick::BuildError),
    #[error("the gitleaks file digest is {found}, but rivethub.toml pins {pinned}")]
    Pin { found: String, pinned: String },
    #[error("rivethub.toml has no [gitleaks] table pinning the vendored file")]
    Unpinned,
    #[error("rivethub.toml has no rule of kind budget to name blobs left undecoded")]
    NoBudget,
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
    names: Option<names::RawNames>,
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
    pub deviations: Vec<String>,
    #[serde(default)]
    pub whole_match: Vec<String>,
    #[serde(default)]
    pub skip: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawRule {
    id: String,
    description: Option<String>,
    kind: Option<Kind>,
    regex: Option<String>,
    path: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    entropy: Option<f64>,
    secret_group: Option<usize>,
    decode: Option<Decode>,
    each: Option<String>,
    family: Option<injection::Family>,
    why: Option<String>,
    #[serde(default)]
    allowlists: Vec<RawAllowlist>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Names,
    Pairs,
    Budget,
    Chars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Decode {
    Base64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Rivethub,
    Gitleaks,
}

#[derive(Debug)]
pub struct Allow {
    target: Target,
    regexes: Vec<Regex>,
    stopwords: Vec<String>,
    all: bool,
}

impl Allow {
    pub(crate) fn exact(regexes: Vec<Regex>) -> Self {
        Self {
            target: Target::Secret,
            regexes,
            stopwords: Vec::new(),
            all: false,
        }
    }
}

#[derive(Debug)]
enum Matcher {
    Regex(Regex),
    Names,
    Pairs,
    Budget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Secret {
    Whole,
    FirstGroup,
    Group(usize),
}

#[derive(Debug)]
pub struct Rule {
    pub id: String,
    pub description: String,
    pub source: Source,
    pub family: Option<injection::Family>,
    pub why: String,
    matcher: Matcher,
    secret: Secret,
    entropy: Option<f64>,
    multiline: bool,
    decode: Option<Decode>,
    each: Option<Regex>,
    allow: Vec<Allow>,
}

#[derive(Debug)]
pub struct Ruleset {
    rules: Vec<Rule>,
    keywords: AhoCorasick,
    keyword_rules: Vec<Vec<usize>>,
    always: Vec<usize>,
    allow_ours: Vec<Allow>,
    allow_theirs: Vec<Allow>,
    names: Names,
    upstream: Option<Upstream>,
    digest: String,
    ours: usize,
    budget: usize,
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
    let ours = parse(RIVETHUB, RIVETHUB_FILE)?;
    let theirs = parse(GITLEAKS, GITLEAKS_FILE)?;
    let upstream = ours.gitleaks.ok_or(RuleError::Unpinned)?;
    let found = hex::encode(Sha256::digest(GITLEAKS.as_bytes()));
    if found != upstream.sha256 {
        return Err(RuleError::Pin {
            found,
            pinned: upstream.sha256,
        });
    }
    let names = Names::build(ours.names.ok_or_else(|| RuleError::SecretNames {
        file: RIVETHUB_FILE,
        message: "missing".to_owned(),
    })?)?;
    let (allow_ours, allow_theirs) = globals(ours.allowlist, theirs.allowlist, &names)?;
    let mut hasher = Sha256::new();
    hasher.update(RIVETHUB.as_bytes());
    hasher.update([0]);
    hasher.update(GITLEAKS.as_bytes());
    assemble(
        Parts {
            sources: vec![
                (ours.rules, Source::Rivethub, RIVETHUB_FILE),
                (theirs.rules, Source::Gitleaks, GITLEAKS_FILE),
            ],
            names,
            allow_ours,
            allow_theirs,
            digest: hex::encode(hasher.finalize()),
        },
        Some(upstream),
    )
}

struct Parts {
    sources: Vec<(Vec<RawRule>, Source, &'static str)>,
    names: Names,
    allow_ours: Vec<Allow>,
    allow_theirs: Vec<Allow>,
    digest: String,
}

fn assemble(parts: Parts, upstream: Option<Upstream>) -> Result<Ruleset, RuleError> {
    let Parts {
        sources,
        mut names,
        allow_ours,
        allow_theirs,
        digest,
    } = parts;
    let skip = upstream.as_ref().map(|upstream| upstream.skip.as_slice());
    let whole_match = upstream
        .as_ref()
        .map(|upstream| upstream.whole_match.as_slice());
    let mut rules = Vec::new();
    let mut patterns: Vec<String> = Vec::new();
    let mut keyword_rules: Vec<Vec<usize>> = Vec::new();
    let mut always = Vec::new();
    let mut ours_count = 0;
    for (raws, source, file) in sources {
        for raw in raws {
            if raw.path.is_some() || skip.is_some_and(|skip| skip.contains(&raw.id)) {
                continue;
            }
            let index = rules.len();
            let keywords = match raw.kind {
                Some(Kind::Names) => names.keywords(),
                Some(Kind::Budget) => Vec::new(),
                Some(Kind::Chars) => vec![ALWAYS.to_owned()],
                Some(Kind::Pairs) | None => raw
                    .keywords
                    .iter()
                    .map(|word| word.to_lowercase())
                    .collect(),
            };
            register(
                index,
                keywords,
                &mut patterns,
                &mut keyword_rules,
                &mut always,
            );
            if source == Source::Rivethub {
                ours_count += 1;
            }
            rules.push(rule(raw, (source, file), whole_match.unwrap_or_default())?);
        }
    }
    names.set_kinds(rules.iter().map(|rule| rule.id.clone()));
    let budget = rules
        .iter()
        .position(|rule| matches!(rule.matcher, Matcher::Budget))
        .ok_or(RuleError::NoBudget)?;
    let keywords = AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::Standard)
        .build(&patterns)?;
    Ok(Ruleset {
        rules,
        keywords,
        keyword_rules,
        always,
        allow_ours,
        allow_theirs,
        names,
        upstream,
        digest,
        ours: ours_count,
        budget,
    })
}

fn globals(
    ours: Option<RawAllowlist>,
    theirs: Option<RawAllowlist>,
    names: &Names,
) -> Result<(Vec<Allow>, Vec<Allow>), RuleError> {
    let mut allow_ours = vec![names.allow()];
    if let Some(raw) = ours
        && let Some(compiled) = allowlist(raw, RIVETHUB_FILE, "allowlist")?
    {
        allow_ours.push(compiled);
    }
    let mut allow_theirs = Vec::new();
    if let Some(raw) = theirs
        && let Some(compiled) = allowlist(raw, GITLEAKS_FILE, "allowlist")?
    {
        allow_theirs.push(compiled);
    }
    Ok((allow_ours, allow_theirs))
}

fn register(
    index: usize,
    keywords: Vec<String>,
    patterns: &mut Vec<String>,
    keyword_rules: &mut Vec<Vec<usize>>,
    always: &mut Vec<usize>,
) {
    for keyword in keywords {
        if keyword == ALWAYS {
            always.push(index);
        } else if let Some(at) = patterns.iter().position(|known| *known == keyword) {
            if let Some(list) = keyword_rules.get_mut(at) {
                list.push(index);
            }
        } else {
            patterns.push(keyword);
            keyword_rules.push(vec![index]);
        }
    }
}

fn parse(text: &str, file: &'static str) -> Result<File, RuleError> {
    toml::from_str(text).map_err(|error| RuleError::Toml {
        file,
        message: error.message().to_owned(),
    })
}

fn rule(
    raw: RawRule,
    (source, file): (Source, &'static str),
    whole_match: &[String],
) -> Result<Rule, RuleError> {
    let id = raw.id;
    let regex_error = |id: &str, error: regex::Error| RuleError::Regex {
        file,
        id: id.to_owned(),
        message: error.to_string(),
    };
    let (matcher, multiline) = match (raw.kind, raw.regex) {
        (Some(Kind::Names), _) => (Matcher::Names, false),
        (Some(Kind::Pairs), _) => (Matcher::Pairs, true),
        (Some(Kind::Budget), _) => (Matcher::Budget, false),
        (kind @ (None | Some(Kind::Chars)), Some(source)) => {
            let regex = compile(&source).map_err(|error| regex_error(&id, error))?;
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
            if kind.is_none() && raw.keywords.is_empty() {
                return Err(RuleError::NoKeywords { file, id });
            }
            let multiline = source.contains("\\n")
                || source.contains("\\r")
                || source.contains("\\s\\S")
                || source.contains("(?s");
            (Matcher::Regex(regex), multiline)
        }
        (Some(Kind::Chars) | None, None) => return Err(RuleError::NoRegex { file, id }),
    };
    let secret = if whole_match.contains(&id) {
        Secret::Whole
    } else {
        raw.secret_group.map_or(Secret::FirstGroup, Secret::Group)
    };
    let each = raw
        .each
        .map(|source| compile(&source).map_err(|error| regex_error(&id, error)))
        .transpose()?;
    let mut allow = Vec::new();
    for raw in raw.allowlists {
        if let Some(compiled) = allowlist(raw, file, &id)? {
            allow.push(compiled);
        }
    }
    Ok(Rule {
        description: raw.description.unwrap_or_default(),
        source,
        family: raw.family,
        why: raw.why.unwrap_or_default(),
        matcher,
        secret,
        entropy: raw.entropy,
        multiline,
        decode: raw.decode,
        each,
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
        .iter()
        .map(|source| compile_allow(source))
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

impl Ruleset {
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    #[must_use]
    pub fn upstream(&self) -> Option<&Upstream> {
        self.upstream.as_ref()
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn names(&self) -> &Names {
        &self.names
    }

    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        (self.ours, self.rules.len() - self.ours)
    }

    #[must_use]
    pub fn is_secret_name(&self, key: &str) -> bool {
        self.names.family(key).is_some()
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
        let (ours, theirs) = rules.counts();
        assert!(theirs > 200, "{theirs} gitleaks rules");
        assert!(ours > 30, "{ours} RivetHub rules");
        assert_eq!(
            rules.upstream().map(|upstream| upstream.version.as_str()),
            Some("v8.30.1")
        );
        assert!(rules.emits("github_token"));
        assert!(rules.emits("github-pat"));
        assert!(rules.emits("secret_assignment"));
        assert!(!rules.emits("ghp_abcdefghij"));
    }
}
