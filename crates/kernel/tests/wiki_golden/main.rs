#![cfg(test)]

mod page;
mod text;

use std::path::PathBuf;

use testkit::wiki_golden::Corpus;

pub(crate) type TestResult = Result<(), Box<dyn std::error::Error>>;

pub(crate) fn corpus() -> Corpus {
    Corpus::new(
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wiki")),
        &[],
    )
}
