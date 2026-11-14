mod alias;
mod bind;
mod types;
mod vectors;

pub(in crate::entity_builder) use bind::resolve_kept;
pub(in crate::entity_builder) use types::settle_prepared_type;
pub(in crate::entity_builder) use vectors::refresh_embedding;
pub(in crate::entity_builder) use vectors::store_vector;

#[cfg(test)]
use super::decide::DecideRequest;
#[cfg(test)]
use super::decide::EntityDecider;
#[cfg(test)]
use super::decide::RawDecision;
#[cfg(test)]
use crate::WorkerError;
#[cfg(test)]
use alias::AliasSubject;
#[cfg(test)]
use alias::RefGate;
#[cfg(test)]
use alias::alias_paired;
#[cfg(test)]
use alias::filter_new_aliases;
#[cfg(test)]
use alias::inject_suggested;
#[cfg(test)]
use alias::new_entity_aliases;
#[cfg(test)]
use alias::ref_gate;
#[cfg(test)]
use bind::Asked;
#[cfg(test)]
use bind::MatchStep;
#[cfg(test)]
use bind::Ruling;
#[cfg(test)]
use bind::apply_asked;
#[cfg(test)]
use bind::ask;
#[cfg(test)]
use bind::decision_for;
#[cfg(test)]
use bind::entity_decision;
#[cfg(test)]
use bind::offered;
#[cfg(test)]
use bind::undecided;
#[cfg(test)]
use kernel::wiki::entity_pass::KnownEntity;
#[cfg(test)]
use kernel::wiki::entity_pass::MatchCandidate;
#[cfg(test)]
use kernel::wiki::entity_pass::MatchDecision;
#[cfg(test)]
use kernel::wiki::entity_pass::PreparedEntity;
#[cfg(test)]
use kernel::wiki::entity_pass::TypeBrief;
#[cfg(test)]
use types::TypeOutcome;
#[cfg(test)]
use types::alias_to_keep;
#[cfg(test)]
use types::settle_type;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
#[path = "tests/entity_resolve_tests.rs"]
mod tests;
