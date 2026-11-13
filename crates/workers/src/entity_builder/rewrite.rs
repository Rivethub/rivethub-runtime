mod report;
mod run;

pub(super) const REWRITE_MAX_TOKENS: u32 = 6_000;

pub use report::EntityExample;
pub use report::EntityRunReport;
pub use report::FactCarryReport;
pub use report::FactExample;
pub use report::MergeExample;
pub use report::RejectedExample;
pub use report::entity_run_report;
pub use report::fact_carry_report;
pub(in crate::entity_builder) use report::record_alias_accepted;
pub(in crate::entity_builder) use report::record_alias_paired;
pub(in crate::entity_builder) use report::record_alias_rejected;
pub(in crate::entity_builder) use report::record_merged;
pub(in crate::entity_builder) use report::record_redirected;
pub(in crate::entity_builder) use report::record_verified_ref;
#[cfg(test)]
pub(in crate::entity_builder) use run::AcceptedRewrite;
pub(in crate::entity_builder) use run::StoredPage;
pub(in crate::entity_builder) use run::TouchedEntity;
#[cfg(test)]
pub(in crate::entity_builder) use run::accepted_rewrite;
#[cfg(test)]
use run::rewrite_prompt;
pub(in crate::entity_builder) use run::rewrite_touched;
#[cfg(test)]
use run::stored_rewrite;

#[cfg(test)]
#[path = "tests/entity_rewrite_tests.rs"]
mod tests;
