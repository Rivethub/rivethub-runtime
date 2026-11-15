use serde::Deserialize;

use crate::json::Json;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub scenarios: Vec<ScenarioRecord>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ScenarioRecord {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub steps: Vec<StepRecord>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StepRecord {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub rows: Option<Json>,
}
