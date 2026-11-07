use std::str::FromStr as _;

use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;
use values::SessionId;

use crate::facts::Evidence;
use crate::facts::FactKey;
use crate::facts::FactValue;
use crate::facts::NewClaim;
use crate::facts::Said;
use crate::owner::Owner;
use crate::topics::TopicId;
use crate::wiki::page::WikiPatch;

pub(super) fn fact_claims(
    entity: TopicId,
    patch: &WikiPatch,
    source: &str,
    summary: Uuid,
    at: DateTime<Utc>,
    owner: Option<&Owner>,
) -> Vec<NewClaim> {
    let Some(facts) = patch.facts.as_deref().filter(|facts| !facts.is_empty()) else {
        return Vec::new();
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B13 (NEW-entities): a writer claim is sourced to a session, so a summary whose source is not a canonical session id writes no facts"
        )
    )]
    let Ok(session) = SessionId::from_str(source) else {
        return Vec::new();
    };
    let mut claims: Vec<NewClaim> = Vec::new();
    for fact in facts {
        let (Ok(key), Ok(value)) = (FactKey::new(&fact.key), FactValue::new(&fact.value)) else {
            continue;
        };
        claims.retain(|claim| claim.key != key);
        claims.push(NewClaim {
            entity,
            key,
            value,
            said: Said::Writer {
                session: session.clone(),
                summary,
                evidence: Evidence::quote(&fact.evidence),
            },
            owner: owner.cloned(),
            at,
        });
    }
    claims
}
