mod alias;
mod classify;
mod screen;

pub use alias::address_tied_to_name;
pub use alias::alias_claims_known;
pub use alias::alias_in_evidence;
pub use alias::alias_is_address;
pub use alias::evidenced_aliases;
pub use alias::merge_partner;
pub use alias::names_share_mention;
pub use classify::Admit;
pub use classify::DropKind;
pub use classify::admit_name;
pub use classify::identifying_aliases;
pub use screen::identified_by;
pub use screen::screen_observations;

#[cfg(test)]
#[path = "tests/entity_admit_tests.rs"]
mod tests;
