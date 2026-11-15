mod alias;
mod bind;
mod types;
mod vectors;

pub(in crate::entity_builder) use bind::resolve_kept;
pub(in crate::entity_builder) use types::settle_prepared_type;
pub(in crate::entity_builder) use vectors::refresh_embedding;
pub(in crate::entity_builder) use vectors::store_vector;

#[cfg(test)]
#[path = "tests/entity_resolve_tests.rs"]
mod tests;
