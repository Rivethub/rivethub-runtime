use kernel::memory::share;
use kernel::memory::share::ChoiceRefusal;
use kernel::memory::share::SessionChoices;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<ChosenSession>")]
pub(crate) struct Chosen(SessionChoices);

impl Chosen {
    pub(crate) fn into_choices(self) -> SessionChoices {
        self.0
    }
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
        let sessions = sessions
            .into_iter()
            .map(|session| {
                let decision = match session.decision {
                    Decision::Approve => share::Decision::Approve,
                    Decision::Redact => share::Decision::Redact,
                };
                let rules = session.redact.map(|rules| share::RedactRules {
                    terms: rules.terms,
                    drop_tool_outputs: rules.drop_tool_outputs,
                });
                (session.id, decision, rules)
            })
            .collect();
        Ok(Self(share::choices(sessions)?))
    }
}
