pub(super) const PASSAGE_TOKEN_BUDGET: usize = 24_000;
const CHARS_PER_TOKEN: usize = 4;
const PASSAGE_CHAR_BUDGET: usize = PASSAGE_TOKEN_BUDGET * CHARS_PER_TOKEN;
const MIN_TERM: usize = 2;
const EXCERPT_CHARS: usize = 4_000;
const WINDOW_LINES: usize = 12;
const CHAR_WINDOW: usize = 1_500;
const FEW_LONG_LINES: usize = 8;

mod budget;
mod excerpt;
mod select;
mod shape;

pub(in crate::entity_builder) use budget::passage_report;
#[cfg(test)]
use excerpt::normalize_map;
pub(in crate::entity_builder) use select::passages;
#[cfg(test)]
use select::passages_within;

#[cfg(test)]
#[path = "tests/entity_gather_tests.rs"]
mod tests;
