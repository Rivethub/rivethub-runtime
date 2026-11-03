use values::HarnessId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessOffering {
    pub harness_id: HarnessId,
    pub chat: bool,
    pub capabilities: HarnessCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCapabilities {
    pub control: HarnessControlCapabilities,
    pub observation: HarnessObservationCapabilities,
    pub turn_options: Option<bool>,
    pub launch_model: Option<bool>,
    pub image_attachments: Option<bool>,
    pub models: Option<Vec<HarnessModelOption>>,
    pub models_source: Option<HarnessModelsSource>,
    pub efforts: Option<Vec<EffortOption>>,
    pub model_flag: Option<String>,
    pub effort_flag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessControlCapabilities {
    pub interrupt: bool,
    pub resume: bool,
    pub approvals: bool,
    pub steering: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessObservationCapabilities {
    pub live_stream: bool,
    pub list_sessions: bool,
    pub drive: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessModelOption {
    pub input_modalities: Option<Vec<String>>,
    pub id: String,
    pub label: String,
    pub is_default: Option<bool>,
    pub efforts: Option<Vec<EffortOption>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessModelsSource {
    Discovered,
    Config,
    Merged,
    Static,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortOption {
    pub id: String,
    pub label: String,
    pub is_default: Option<bool>,
}

impl HarnessCapabilities {
    #[must_use]
    pub fn offered_model<'a>(&'a self, recorded: &'a str) -> &'a str {
        let Some(models) = self.models.as_deref() else {
            return recorded;
        };
        if models.iter().any(|model| model.id == recorded) {
            return recorded;
        }
        let parts: Vec<&str> = recorded
            .split(|c: char| !c.is_ascii_alphanumeric())
            .collect();
        let mut named = models.iter().filter(|model| {
            parts
                .iter()
                .any(|part| part.eq_ignore_ascii_case(&model.id))
        });
        match (named.next(), named.next()) {
            (Some(model), None) => &model.id,
            _ => recorded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HarnessObservationCapabilities;
    use super::{HarnessCapabilities, HarnessControlCapabilities, HarnessModelOption};

    fn offering(ids: &[&str]) -> HarnessCapabilities {
        HarnessCapabilities {
            control: HarnessControlCapabilities {
                interrupt: true,
                resume: true,
                approvals: true,
                steering: None,
            },
            observation: HarnessObservationCapabilities {
                live_stream: true,
                list_sessions: true,
                drive: None,
            },
            turn_options: None,
            launch_model: None,
            image_attachments: None,
            models: Some(
                ids.iter()
                    .map(|id| HarnessModelOption {
                        input_modalities: None,
                        id: (*id).to_owned(),
                        label: (*id).to_owned(),
                        is_default: None,
                        efforts: None,
                    })
                    .collect(),
            ),
            models_source: None,
            efforts: None,
            model_flag: None,
            effort_flag: None,
        }
    }

    #[test]
    fn a_recorded_model_is_named_by_the_one_offered_id_within_it() {
        let claude = offering(&["default", "opus", "sonnet", "haiku"]);
        assert_eq!(claude.offered_model("claude-haiku-5-5"), "haiku");
        assert_eq!(claude.offered_model("sonnet"), "sonnet");
        assert_eq!(claude.offered_model("gpt-5"), "gpt-5");
        let twins = offering(&["opus", "haiku"]);
        assert_eq!(twins.offered_model("opus-haiku-1"), "opus-haiku-1");
    }
}
