use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    Display,
    EnumString,
    EnumIter,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    GrokBuild,
    KimiCode,
    Hermes,
    Codex,
    Opencode,
    Omp,
    Dsh,
    Cursor,
    Cowork,
    Antigravity,
}
