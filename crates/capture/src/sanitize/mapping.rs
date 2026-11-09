use std::ops::Range;

use aho_corasick::AhoCorasick;
use aho_corasick::MatchKind;
use regex::Regex;
use regex::RegexBuilder;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use toml::Spanned;

#[derive(Debug)]
pub struct Rule {
    pub line: usize,
    pub matcher: Matcher,
    pub action: Action,
}

#[derive(Debug)]
pub enum Matcher {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", content = "as", rename_all = "snake_case")]
pub enum Action {
    Map(String),
    Redact,
}

pub const REDACTED: &str = "[REDACTED]";
const COMBINED_SIZE_LIMIT: usize = 32 << 20;

impl Action {
    #[must_use]
    pub fn replacement(&self) -> &str {
        match self {
            Self::Map(placeholder) => placeholder,
            Self::Redact => REDACTED,
        }
    }
}

#[derive(Debug, Default)]
pub struct Mapping {
    rules: Vec<Rule>,
    literals: Option<AhoCorasick>,
    literal_rules: Vec<usize>,
    regexes: Option<Regex>,
    regex_groups: Vec<(usize, usize)>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MappingError {
    #[error("mapping file is not valid TOML at line {line}: {message}")]
    Toml { line: usize, message: String },
    #[error("mapping rule at line {line} {problem}")]
    Rule { line: usize, problem: RuleProblem },
    #[error("mapping rule at line {line} has a regex that does not compile: {message}")]
    Regex { line: usize, message: String },
    #[error("the mapping rules do not combine into one pattern: {message}")]
    Combined { message: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuleProblem {
    #[error("sets both literal and regex; give one")]
    BothMatchers,
    #[error("sets neither literal nor regex")]
    NoMatcher,
    #[error("has an empty literal or regex")]
    EmptyMatcher,
    #[error("maps without a placeholder; add `as = \"<placeholder>\"`")]
    MapWithoutPlaceholder,
    #[error("redacts but also sets `as`; use action = \"map\" for a placeholder")]
    RedactWithPlaceholder,
    #[error("has an empty placeholder")]
    EmptyPlaceholder,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    rule: Vec<Spanned<RawRule>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    literal: Option<String>,
    regex: Option<String>,
    action: RawAction,
    #[serde(rename = "as")]
    placeholder: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawAction {
    Map,
    Redact,
}

pub fn parse(text: &str) -> Result<Mapping, MappingError> {
    let file: File = toml::from_str(text).map_err(|error| MappingError::Toml {
        line: error.span().map_or(1, |span| line_of(text, span.start)),
        message: error.message().to_owned(),
    })?;
    let rules = file
        .rule
        .into_iter()
        .map(|spanned| rule(line_of(text, spanned.span().start), spanned.into_inner()))
        .collect::<Result<Vec<Rule>, MappingError>>()?;
    Mapping::compile(rules)
}

fn rule(line: usize, raw: RawRule) -> Result<Rule, MappingError> {
    let problem = |problem| MappingError::Rule { line, problem };
    let matcher = match (raw.literal, raw.regex) {
        (Some(_), Some(_)) => return Err(problem(RuleProblem::BothMatchers)),
        (None, None) => return Err(problem(RuleProblem::NoMatcher)),
        (Some(literal), None) if literal.is_empty() => {
            return Err(problem(RuleProblem::EmptyMatcher));
        }
        (None, Some(source)) if source.is_empty() => {
            return Err(problem(RuleProblem::EmptyMatcher));
        }
        (Some(literal), None) => Matcher::Literal(literal),
        (None, Some(source)) => {
            Matcher::Regex(Regex::new(&source).map_err(|error| MappingError::Regex {
                line,
                message: error.to_string(),
            })?)
        }
    };
    let action = match (raw.action, raw.placeholder) {
        (RawAction::Map, None) => return Err(problem(RuleProblem::MapWithoutPlaceholder)),
        (RawAction::Map, Some(placeholder)) if placeholder.is_empty() => {
            return Err(problem(RuleProblem::EmptyPlaceholder));
        }
        (RawAction::Map, Some(placeholder)) => Action::Map(placeholder),
        (RawAction::Redact, Some(_)) => return Err(problem(RuleProblem::RedactWithPlaceholder)),
        (RawAction::Redact, None) => Action::Redact,
    };
    Ok(Rule {
        line,
        matcher,
        action,
    })
}

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |before| before.matches('\n').count() + 1)
}

impl Mapping {
    pub fn compile(rules: Vec<Rule>) -> Result<Self, MappingError> {
        let mut literals: Vec<&str> = Vec::new();
        let mut literal_rules = Vec::new();
        let mut alternatives: Vec<String> = Vec::new();
        let mut regex_groups = Vec::new();
        let mut next_group = 1;
        for (index, rule) in rules.iter().enumerate() {
            match &rule.matcher {
                Matcher::Literal(literal) => {
                    literals.push(literal);
                    literal_rules.push(index);
                }
                Matcher::Regex(regex) => {
                    alternatives.push(format!("({})", regex.as_str()));
                    regex_groups.push((index, next_group));
                    next_group += regex.captures_len();
                }
            }
        }
        let automaton = if literals.is_empty() {
            None
        } else {
            Some(
                AhoCorasick::builder()
                    .match_kind(MatchKind::LeftmostFirst)
                    .build(&literals)
                    .map_err(|error| MappingError::Combined {
                        message: error.to_string(),
                    })?,
            )
        };
        let combined = if alternatives.is_empty() {
            None
        } else {
            Some(
                RegexBuilder::new(&alternatives.join("|"))
                    .size_limit(COMBINED_SIZE_LIMIT)
                    .build()
                    .map_err(|error| MappingError::Combined {
                        message: error.to_string(),
                    })?,
            )
        };
        Ok(Self {
            rules,
            literals: automaton,
            literal_rules,
            regexes: combined,
            regex_groups,
        })
    }

    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(crate) fn spans(&self, text: &str) -> Vec<(Range<usize>, &Rule)> {
        let mut found: Vec<(Range<usize>, usize)> = Vec::new();
        if let Some(automaton) = &self.literals {
            for hit in automaton.find_iter(text) {
                if let Some(&rule) = self.literal_rules.get(hit.pattern().as_usize()) {
                    found.push((hit.range(), rule));
                }
            }
        }
        if let Some(combined) = &self.regexes {
            for captures in combined.captures_iter(text) {
                let hit = self.regex_groups.iter().find_map(|&(rule, group)| {
                    captures.get(group).map(|found| (found.range(), rule))
                });
                if let Some((range, rule)) = hit
                    && !range.is_empty()
                {
                    found.push((range, rule));
                }
            }
        }
        found.sort_by_key(|(range, rule)| (range.start, *rule));
        let mut kept: Vec<(Range<usize>, &Rule)> = Vec::new();
        for (range, index) in found {
            if kept.last().is_some_and(|(last, _)| range.start < last.end) {
                continue;
            }
            if let Some(rule) = self.rules.get(index) {
                kept.push((range, rule));
            }
        }
        kept
    }
}
