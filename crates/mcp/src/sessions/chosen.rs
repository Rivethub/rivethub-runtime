use kernel::memory::share::Choice;
use kernel::memory::share::ChoiceError;
use kernel::memory::share::SessionChoices;
use kernel::memory::share::SessionRedaction;
use kernel::memory::share::Term;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<ChosenSession>")]
pub(crate) struct Chosen(SessionChoices);

impl Chosen {
    pub(crate) fn into_choices(self) -> SessionChoices {
        self.0
    }
}

#[derive(Debug, Error)]
pub(crate) enum ChoiceRefusal {
    #[error(
        "session {0} is approved, so it takes no redact rules; set decision to redact to apply them"
    )]
    ApprovedWithRules(Uuid),
    #[error("session {id}: {source}")]
    Rules { id: Uuid, source: ChoiceError },
    #[error(transparent)]
    Choices(#[from] ChoiceError),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChosenSession {
    #[schemars(description = "A conversation id from session_find", with = "String")]
    id: Uuid,
    #[schemars(
        description = "approve shares the session as the sanitizer leaves it; redact also applies the rules in redact"
    )]
    decision: Decision,
    #[schemars(description = "Required with decision redact, refused with approve: at least one term or drop_tool_outputs true", transform = crate::catalog::optional_non_null)]
    redact: Option<Rules>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Approve,
    Redact,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Rules {
    #[schemars(description = "Texts to redact in this session only, each matched exactly as written (case-sensitive); the report counts them without showing them", transform = crate::catalog::optional_non_null)]
    terms: Option<Vec<String>>,
    #[schemars(description = "Replace every tool output of this session with a placeholder; tool names and arguments stay and are sanitized as usual", transform = crate::catalog::optional_non_null)]
    drop_tool_outputs: Option<bool>,
}

impl TryFrom<Vec<ChosenSession>> for Chosen {
    type Error = ChoiceRefusal;

    fn try_from(sessions: Vec<ChosenSession>) -> Result<Self, Self::Error> {
        let chosen = sessions
            .into_iter()
            .map(|session| {
                let id = session.id;
                let choice = match (session.decision, session.redact) {
                    (Decision::Approve, None) => Choice::Approve,
                    (Decision::Approve, Some(_)) => {
                        return Err(ChoiceRefusal::ApprovedWithRules(id));
                    }
                    (Decision::Redact, rules) => {
                        let Rules {
                            terms,
                            drop_tool_outputs,
                        } = rules.unwrap_or_default();
                        let redaction = terms
                            .unwrap_or_default()
                            .into_iter()
                            .map(Term::try_from)
                            .collect::<Result<Vec<_>, _>>()
                            .and_then(|terms| {
                                SessionRedaction::new(terms, drop_tool_outputs == Some(true))
                            })
                            .map_err(|source| ChoiceRefusal::Rules { id, source })?;
                        Choice::Redact(redaction)
                    }
                };
                Ok((id, choice))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(SessionChoices::try_from(chosen)?))
    }
}
