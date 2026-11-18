use jiff::Timestamp;
use std::num::NonZeroU32;
use std::path::Path;

use kernel::memory::SearchMode;
use serde::Deserialize;
use serde::Serialize;

use crate::Failure;
use crate::read;

#[serde_with::serde_as]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuerySet {
    #[serde_as(as = "Option<wire::time::Rfc3339>")]
    pub(crate) clock: Option<Timestamp>,
    pub(crate) limit: NonZeroU32,
    #[serde(default)]
    pub(crate) tolerance: f64,
    pub(crate) queries: Vec<EvalQuery>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvalQuery {
    pub(crate) id: String,
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) mode: SearchMode,
    #[serde(default)]
    pub(crate) relevant: Vec<String>,
    #[serde(default)]
    pub(crate) must_contain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Baseline {
    pub(crate) limit: u32,
    pub(crate) query_count: u32,
    pub(crate) recall_at_k: f64,
    pub(crate) mrr: f64,
    pub(crate) must_contain_total: u32,
    pub(crate) must_contain_passed: u32,
    pub(crate) must_contain_pass_rate: f64,
}

impl QuerySet {
    pub(crate) fn load(path: &Path) -> Result<Self, Failure> {
        parse(path)
    }
}

impl Baseline {
    pub(crate) fn load(path: &Path) -> Result<Self, Failure> {
        parse(path)
    }

    pub(crate) fn save(path: &Path, baseline: &Self) -> Result<(), Failure> {
        let mut text = serde_json::to_string_pretty(baseline).map_err(Failure::Encode)?;
        text.push('\n');
        std::fs::write(path, text).map_err(|source| Failure::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Failure> {
    let text = std::fs::read_to_string(path).map_err(read(path))?;
    serde_json::from_str(&text).map_err(|source| Failure::Parse {
        path: path.to_path_buf(),
        source,
    })
}
