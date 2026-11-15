use std::path::Path;

use serde::Deserialize;

use crate::Error;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EntryRecord {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub stdin: String,
}

pub fn entries(recording: &Path) -> Result<Vec<EntryRecord>, Error> {
    let raw = crate::read_text(recording)?;
    crate::parse_json(recording, &raw)
}
