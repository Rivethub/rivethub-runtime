use std::collections::BTreeMap;

use kernel::memory::share::SessionRedaction;
use serde::Serialize;

use super::mapping::Action;
use super::mapping::Mapping;
use super::mapping::MappingError;
use super::mapping::Matcher;
use super::mapping::Rule;
use crate::rules::names::SESSION_TERM_KIND;

pub(super) static NONE: SessionRules = SessionRules(BTreeMap::new());

#[derive(Debug, Default)]
pub struct SessionRules(BTreeMap<usize, SessionRule>);

#[derive(Debug)]
struct SessionRule {
    redaction: SessionRedaction,
    terms: Mapping,
}

#[derive(Serialize)]
pub(super) struct SessionText<'a> {
    session: usize,
    terms: Vec<&'a str>,
    drop_tool_outputs: bool,
}

impl SessionRules {
    pub fn new(sessions: &BTreeMap<usize, SessionRedaction>) -> Result<Self, MappingError> {
        let placeholder = crate::redaction::placeholder(SESSION_TERM_KIND);
        sessions
            .iter()
            .map(|(index, redaction)| {
                let rules = redaction
                    .terms()
                    .iter()
                    .enumerate()
                    .map(|(position, term)| Rule {
                        line: position + 1,
                        matcher: Matcher::Literal(term.as_str().to_owned()),
                        action: Action::Map(placeholder.clone()),
                    })
                    .collect();
                Ok((
                    *index,
                    SessionRule {
                        redaction: redaction.clone(),
                        terms: Mapping::compile(rules)?,
                    },
                ))
            })
            .collect::<Result<_, MappingError>>()
            .map(Self)
    }

    pub(super) fn terms(&self, session: usize) -> Option<&Mapping> {
        self.0
            .get(&session)
            .map(|rule| &rule.terms)
            .filter(|terms| !terms.is_empty())
    }

    pub(super) fn drops_tool_outputs(&self, session: usize) -> bool {
        self.0
            .get(&session)
            .is_some_and(|rule| rule.redaction.drops_tool_outputs())
    }

    pub(super) fn text(&self) -> Vec<SessionText<'_>> {
        self.0
            .iter()
            .map(|(session, rule)| SessionText {
                session: *session,
                terms: rule
                    .redaction
                    .terms()
                    .iter()
                    .map(kernel::memory::share::Term::as_str)
                    .collect(),
                drop_tool_outputs: rule.redaction.drops_tool_outputs(),
            })
            .collect()
    }
}
