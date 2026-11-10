use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;

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

#[expect(
    clippy::expect_used,
    reason = "a constant class; every mapping test compiles it"
)]
static NON_ASCII_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w&&[^\x00-\x7F]]").expect("the class compiles"));

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
    regexes: Vec<RuleRegex>,
}

#[derive(Debug)]
struct RuleRegex {
    rule: usize,
    on_ascii: Regex,
    on_unicode: Regex,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MappingError {
    #[error("mapping file is not valid TOML at line {line}: {message}")]
    Toml { line: usize, message: String },
    #[error("mapping rule at line {line} {problem}")]
    Rule { line: usize, problem: RuleProblem },
    #[error("mapping rule at line {line} has a regex that does not compile: {message}")]
    Regex { line: usize, message: String },
    #[error("a mapping regex does not compile for the scan: {message}")]
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

#[must_use]
pub fn map_rule(literal: &str, placeholder: &str) -> String {
    let quote = |text: &str| toml::Value::String(text.to_owned()).to_string();
    format!(
        "[[rule]]\nliteral = {}\naction = \"map\"\nas = {}\n",
        quote(literal),
        quote(placeholder)
    )
}

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |before| before.matches('\n').count() + 1)
}

fn ascii_boundaries(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 16);
    let mut chars = source.chars();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    out.push(c);
                    break;
                };
                if !in_class && (next == 'b' || next == 'B') {
                    out.push_str("(?-u:\\");
                    out.push(next);
                    out.push(')');
                } else {
                    out.push(c);
                    out.push(next);
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
            _ => out.push(c),
        }
    }
    out
}

impl Mapping {
    pub fn compile(rules: Vec<Rule>) -> Result<Self, MappingError> {
        let mut literals: Vec<&str> = Vec::new();
        let mut literal_rules = Vec::new();
        let mut regexes = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            match &rule.matcher {
                Matcher::Literal(literal) => {
                    literals.push(literal);
                    literal_rules.push(index);
                }
                Matcher::Regex(regex) => {
                    let on_ascii = RegexBuilder::new(&ascii_boundaries(regex.as_str()))
                        .size_limit(COMBINED_SIZE_LIMIT)
                        .build()
                        .map_err(|error| MappingError::Combined {
                            message: error.to_string(),
                        })?;
                    regexes.push(RuleRegex {
                        rule: index,
                        on_ascii,
                        on_unicode: regex.clone(),
                    });
                }
            }
        }
        let automaton = if literals.is_empty() {
            None
        } else {
            Some(
                AhoCorasick::builder()
                    .match_kind(MatchKind::Standard)
                    .build(&literals)
                    .map_err(|error| MappingError::Combined {
                        message: error.to_string(),
                    })?,
            )
        };
        Ok(Self {
            rules,
            literals: automaton,
            literal_rules,
            regexes,
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
        let mut found: Vec<(usize, Range<usize>)> = Vec::new();
        if let Some(automaton) = &self.literals {
            for hit in automaton.find_overlapping_iter(text) {
                if let Some(&rule) = self.literal_rules.get(hit.pattern().as_usize()) {
                    found.push((rule, hit.range()));
                }
            }
        }
        let ascii_words = text.is_ascii() || !NON_ASCII_WORD.is_match(text);
        for compiled in &self.regexes {
            let regex = if ascii_words {
                &compiled.on_ascii
            } else {
                &compiled.on_unicode
            };
            found.extend(
                regex
                    .find_iter(text)
                    .filter(|hit| !hit.is_empty())
                    .map(|hit| (compiled.rule, hit.range())),
            );
        }
        found.sort_by_key(|(rule, range)| (*rule, range.start));
        let mut taken: BTreeMap<usize, (Range<usize>, usize)> = BTreeMap::new();
        for (rule, range) in found {
            let overlaps = taken
                .range(..range.end)
                .next_back()
                .is_some_and(|(_, (kept, _))| range.start < kept.end);
            if !overlaps {
                taken.insert(range.start, (range, rule));
            }
        }
        taken
            .into_values()
            .filter_map(|(range, index)| self.rules.get(index).map(|rule| (range, rule)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_boundaries_in_mapping_regexes_become_ascii() {
        assert_eq!(
            ascii_boundaries(r"\b\d{3}-\d{4}\b"),
            r"(?-u:\b)\d{3}-\d{4}(?-u:\b)"
        );
        assert_eq!(ascii_boundaries(r"[\b]x\\b\Bq"), r"[\b]x\\b(?-u:\B)q");
    }
}
