use std::sync::LazyLock;

use serde::Deserialize;
use sha2::Digest as _;
use sha2::Sha256;

use super::Budget;
use super::Parts;
use super::RuleError;
use super::Ruleset;
use super::Source;
use super::Span;
use super::assemble;
use super::globals;
use super::names::Names;
use super::parse;

const INJECTION: &str = include_str!("injection.toml");
const INJECTION_FILE: &str = "injection.toml";
pub(crate) const BUDGET_EXHAUSTED: &str = "scan_budget_exhausted";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, strum::Display)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Family {
    Override,
    RoleMarkup,
    HiddenUnicode,
    Exfil,
    PipeShell,
    Encoded,
    Budget,
}

#[expect(
    clippy::expect_used,
    reason = "injection.toml is an embedded constant; every_injection_rule_compiles runs load() in CI"
)]
static RULESET: LazyLock<Ruleset> =
    LazyLock::new(|| load().expect("the embedded injection rules compile"));

#[must_use]
pub fn ruleset() -> &'static Ruleset {
    &RULESET
}

pub(crate) fn spans(text: &str, budget: &mut Budget<'_>) -> Vec<(Span<'static>, Family)> {
    ruleset()
        .spans_within(text, budget)
        .into_iter()
        .filter_map(|span| span.rule.family.map(|family| (span, family)))
        .collect()
}

pub fn load() -> Result<Ruleset, RuleError> {
    let file = parse(INJECTION, INJECTION_FILE)?;
    let names = Names::empty()?;
    let (allow_ours, allow_theirs) = globals(file.allowlist, None, &names)?;
    for raw in &file.rules {
        if raw.family.is_none() || raw.why.as_deref().is_none_or(str::is_empty) {
            return Err(RuleError::NoFamily {
                file: INJECTION_FILE,
                id: raw.id.clone(),
            });
        }
    }
    assemble(
        Parts {
            sources: vec![(file.rules, Source::Rivethub, INJECTION_FILE)],
            names,
            allow_ours,
            allow_theirs,
            digest: hex::encode(Sha256::digest(INJECTION.as_bytes())),
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_injection_rule_compiles() {
        let rules = load().unwrap_or_else(|error| panic!("{error}"));
        assert!(rules.rules().len() > 10);
        assert!(rules.upstream().is_none());
        assert!(
            rules
                .rules()
                .iter()
                .any(|rule| rule.id == BUDGET_EXHAUSTED && rule.family == Some(Family::Budget))
        );
    }
}
