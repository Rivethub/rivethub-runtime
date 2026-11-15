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
