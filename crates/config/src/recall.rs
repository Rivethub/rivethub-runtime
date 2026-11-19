use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecallSection {
    pub own_weight: Option<f64>,
}

impl RecallSection {
    #[must_use]
    pub fn weight(&self) -> f64 {
        self.own_weight.unwrap_or(1.25)
    }
}
