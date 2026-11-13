mod alias;
mod bind;
mod vectors;

pub(in crate::entity_builder) use bind::resolve_kept;
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
use bind::MatchStep;
#[cfg(test)]
use bind::ask;
#[cfg(test)]
use bind::decision_for;
#[cfg(test)]
use bind::interpret;
#[cfg(test)]
use bind::offered;
#[cfg(test)]
use kernel::wiki::entity_pass::KnownEntity;
#[cfg(test)]
use kernel::wiki::entity_pass::MatchCandidate;
#[cfg(test)]
use kernel::wiki::entity_pass::PreparedEntity;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
#[path = "tests/entity_resolve_tests.rs"]
mod tests;
