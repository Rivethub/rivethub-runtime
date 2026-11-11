pub use wire::harness::HarnessId;
pub use wire::levels::Effort;
pub use wire::levels::LogLevel;
pub use wire::stream::ThinkingLevel;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum DeploymentTarget {
    Docker,
    Proxmox,
    Kubernetes,
    Manual,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Isolation {
    Inherit,
    Isolated,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ModelsMode {
    Discover,
    Replace,
    Merge,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum MemoryBackend {
    Postgres,
    Sqlite,
}

pub(crate) fn listed<T: std::fmt::Display + strum::IntoEnumIterator>() -> String {
    T::iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
