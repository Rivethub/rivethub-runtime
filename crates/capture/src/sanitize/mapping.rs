use regex::Regex;
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

impl Action {
    #[must_use]
    pub fn replacement(&self) -> &str {
        match self {
            Self::Map(placeholder) => placeholder,
            Self::Redact => REDACTED,
        }
    }
}

impl Matcher {
    pub(crate) fn spans(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        match self {
            Self::Literal(literal) => text
                .match_indices(literal.as_str())
                .map(|(start, found)| start..start + found.len())
                .collect(),
            Self::Regex(regex) => regex
                .find_iter(text)
                .map(|found| found.range())
                .filter(|range| !range.is_empty())
                .collect(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MappingError {
    #[error("mapping file is not valid TOML at line {line}: {message}")]
    Toml { line: usize, message: String },
    #[error("mapping rule at line {line} {problem}")]
    Rule { line: usize, problem: RuleProblem },
    #[error("mapping rule at line {line} has a regex that does not compile: {message}")]
    Regex { line: usize, message: String },
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
    regex: Option<Spanned<String>>,
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

pub fn parse(text: &str) -> Result<Vec<Rule>, MappingError> {
    let file: File = toml::from_str(text).map_err(|error| MappingError::Toml {
        line: error.span().map_or(1, |span| line_of(text, span.start)),
        message: error.message().to_owned(),
    })?;
    file.rule
        .into_iter()
        .map(|spanned| {
            let line = line_of(text, spanned.span().start);
            rule(line, text, spanned.into_inner())
        })
        .collect()
}

fn rule(line: usize, text: &str, raw: RawRule) -> Result<Rule, MappingError> {
    let problem = |problem| MappingError::Rule { line, problem };
    let matcher = match (raw.literal, raw.regex) {
        (Some(_), Some(_)) => return Err(problem(RuleProblem::BothMatchers)),
        (None, None) => return Err(problem(RuleProblem::NoMatcher)),
        (Some(literal), None) if literal.is_empty() => {
            return Err(problem(RuleProblem::EmptyMatcher));
        }
        (None, Some(source)) if source.get_ref().is_empty() => {
            return Err(problem(RuleProblem::EmptyMatcher));
        }
        (Some(literal), None) => Matcher::Literal(literal),
        (None, Some(source)) => {
            let regex_line = line_of(text, source.span().start);
            Matcher::Regex(
                Regex::new(source.get_ref()).map_err(|error| MappingError::Regex {
                    line: regex_line,
                    message: error.to_string(),
                })?,
            )
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
