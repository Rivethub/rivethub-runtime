mod catalog;
mod resolve;

pub use catalog::VocabEntry;
pub use catalog::vocabulary;
pub use resolve::canonical_attribute;
pub use resolve::vocabulary_key;

#[cfg(test)]
#[path = "tests/entity_vocab_tests.rs"]
mod tests;
