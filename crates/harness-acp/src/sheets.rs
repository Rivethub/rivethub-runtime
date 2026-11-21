use std::collections::BTreeMap;
use std::collections::BTreeSet;

use config::Config;
use config::Effort;
use config::ModelsMode;
use kernel::harness::EffortOption;
use kernel::harness::HarnessCapabilities;
use kernel::harness::HarnessControlCapabilities;
use kernel::harness::HarnessModelOption;
use kernel::harness::HarnessModelsSource;
use kernel::harness::HarnessObservationCapabilities;
use kernel::harness::HarnessOffering;
use thiserror::Error;
use wire::harness::HarnessId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SheetInput {
    pub(crate) models: Option<Vec<String>>,
    pub(crate) efforts: Option<Vec<Effort>>,
    pub(crate) mode: Option<ModelsMode>,
}

#[derive(Debug, Error)]
pub(crate) enum SheetError {
    #[error("model token {token} is not allowed for {harness}")]
    ModelToken { harness: HarnessId, token: String },
}

struct StaticSheet {
    model_flag: Option<&'static str>,
    effort_flag: Option<&'static str>,
    launch_model: bool,
    models: &'static [&'static str],
    default_model: Option<&'static str>,
    publish_empty: bool,
}

pub(crate) fn offering(
    harness: HarnessId,
    chat: bool,
    input: Option<&SheetInput>,
) -> Result<HarnessOffering, SheetError> {
    let sheet = static_sheet(harness);
    let resolved = resolve(harness, &sheet, input)?;
    Ok(HarnessOffering {
        harness_id: harness,
        chat,
        capabilities: capabilities(chat, &sheet, &resolved),
    })
}

struct Resolved {
    models: Option<Vec<HarnessModelOption>>,
    efforts: Option<Vec<EffortOption>>,
    source: Option<HarnessModelsSource>,
}

fn resolve(
    harness: HarnessId,
    sheet: &StaticSheet,
    input: Option<&SheetInput>,
) -> Result<Resolved, SheetError> {
    let mode = input.and_then(|input| input.mode);
    let model_list = listed(input.and_then(|input| input.models.as_deref()), mode);
    let effort_list = listed(input.and_then(|input| input.efforts.as_deref()), mode);
    let models = match model_list {
        Listed::Static => static_models(sheet),
        Listed::Replace(models) => Some(config_models(harness, models)?),
        Listed::Merge(models) => Some(merge_models(harness, sheet, models)?),
    };
    let efforts = match effort_list {
        Listed::Static => None,
        Listed::Replace(efforts) | Listed::Merge(efforts) => Some(effort_options(efforts)),
    };
    let source = source_of(
        &model_list,
        &effort_list,
        models.is_some(),
        efforts.is_some(),
        !sheet.models.is_empty(),
    );
    Ok(Resolved {
        models,
        efforts,
        source,
    })
}

#[derive(Clone, Copy)]
enum Listed<'a, T> {
    Static,
    Replace(&'a [T]),
    Merge(&'a [T]),
}

fn listed<T>(values: Option<&[T]>, mode: Option<ModelsMode>) -> Listed<'_, T> {
    let Some(values) = values.filter(|values| !values.is_empty()) else {
        return Listed::Static;
    };
    match mode {
        Some(ModelsMode::Discover) => Listed::Static,
        Some(ModelsMode::Replace) => Listed::Replace(values),
        Some(ModelsMode::Merge) | None => Listed::Merge(values),
    }
}

fn source_of(
    model_list: &Listed<'_, String>,
    effort_list: &Listed<'_, Effort>,
    has_models: bool,
    has_efforts: bool,
    static_models_present: bool,
) -> Option<HarnessModelsSource> {
    let model_source = contribution(model_list, has_models, static_models_present);
    let effort_source = contribution(effort_list, has_efforts, false);
    match (model_source, effort_source) {
        (Contribution::None, Contribution::None) => None,
        (Contribution::None, other) | (other, Contribution::None) => Some(other.into_source()),
        (left, right) => Some(left.combine(right).into_source()),
    }
}

enum Contribution {
    None,
    Static,
    Config,
    Mixed,
}

impl Contribution {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Static, Self::Static) => Self::Static,
            (Self::Config, Self::Config) => Self::Config,
            (Self::None, value) | (value, Self::None) => value,
            _ => Self::Mixed,
        }
    }

    fn into_source(self) -> HarnessModelsSource {
        match self {
            Self::None | Self::Static => HarnessModelsSource::Static,
            Self::Config => HarnessModelsSource::Config,
            Self::Mixed => HarnessModelsSource::Merged,
        }
    }
}

fn contribution<T>(list: &Listed<'_, T>, produced: bool, static_present: bool) -> Contribution {
    if !produced {
        return Contribution::None;
    }
    match list {
        Listed::Merge(_) if static_present => Contribution::Mixed,
        Listed::Static => Contribution::Static,
        Listed::Replace(_) | Listed::Merge(_) => Contribution::Config,
    }
}

fn static_models(sheet: &StaticSheet) -> Option<Vec<HarnessModelOption>> {
    if sheet.models.is_empty() {
        return sheet.publish_empty.then_some(Vec::new());
    }
    Some(
        sheet
            .models
            .iter()
            .map(|id| model_option(id, sheet.default_model == Some(*id)))
            .collect(),
    )
}

fn config_models(
    harness: HarnessId,
    models: &[String],
) -> Result<Vec<HarnessModelOption>, SheetError> {
    let mut out = Vec::with_capacity(models.len());
    let mut seen = BTreeSet::new();
    for token in models {
        push_model(harness, token, false, &mut seen, &mut out)?;
    }
    Ok(out)
}

fn merge_models(
    harness: HarnessId,
    sheet: &StaticSheet,
    models: &[String],
) -> Result<Vec<HarnessModelOption>, SheetError> {
    let mut out = static_models(sheet)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut seen = out
        .iter()
        .map(|model| model.id.clone())
        .collect::<BTreeSet<_>>();
    for token in models {
        push_model(harness, token, false, &mut seen, &mut out)?;
    }
    Ok(out)
}

fn push_model(
    harness: HarnessId,
    token: &str,
    default: bool,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<HarnessModelOption>,
) -> Result<(), SheetError> {
    if !model_token(token) {
        return Err(SheetError::ModelToken {
            harness,
            token: token.to_owned(),
        });
    }
    if seen.insert(token.to_owned()) {
        out.push(model_option(token, default));
    }
    Ok(())
}

fn model_option(id: &str, default: bool) -> HarnessModelOption {
    HarnessModelOption {
        input_modalities: None,
        id: id.to_owned(),
        label: id.to_owned(),
        is_default: default.then_some(true),
        efforts: None,
    }
}

fn effort_options(efforts: &[Effort]) -> Vec<EffortOption> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for effort in efforts {
        let id = effort.to_string();
        if seen.insert(id.clone()) {
            out.push(EffortOption {
                id: id.clone(),
                label: id,
                is_default: None,
            });
        }
    }
    out
}

pub(crate) fn model_token(value: &str) -> bool {
    let Some(first) = value.chars().next() else {
        return false;
    };
    if first == '~' || value.contains("..") {
        return false;
    }
    value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || matches!(character, '/' | '~' | '-' | '_' | '.' | ':' | '[' | ']')
    })
}

fn capabilities(chat: bool, sheet: &StaticSheet, resolved: &Resolved) -> HarnessCapabilities {
    HarnessCapabilities {
        control: HarnessControlCapabilities {
            interrupt: chat,
            resume: chat,
            approvals: chat,
            steering: None,
        },
        observation: HarnessObservationCapabilities {
            live_stream: chat,
            list_sessions: false,
            drive: Some(chat),
        },
        turn_options: chat.then_some(true),
        launch_model: sheet.launch_model.then_some(true),
        image_attachments: None,
        models: resolved.models.clone(),
        models_source: resolved.source,
        efforts: resolved.efforts.clone(),
        model_flag: sheet.model_flag.map(str::to_owned),
        effort_flag: sheet.effort_flag.map(str::to_owned),
    }
}

fn static_sheet(harness: HarnessId) -> StaticSheet {
    match harness {
        HarnessId::ClaudeCode => claude_sheet(),
        HarnessId::GrokBuild => grok_sheet(),
        HarnessId::Hermes => flagged("-m", Some("--reasoning")),
        HarnessId::KimiCode => flagged("--model", None),
        HarnessId::Codex => flagged("--model", Some("-c")),
        HarnessId::Opencode => flagged("--model", Some("--variant")),
        HarnessId::Omp => flagged("--model", Some("--thinking")),
        HarnessId::Cursor => cursor_sheet(),
        HarnessId::Dsh | HarnessId::Cowork | HarnessId::Antigravity => flagged_empty(),
    }
}

fn claude_sheet() -> StaticSheet {
    StaticSheet {
        model_flag: Some("--model"),
        effort_flag: Some("--effort"),
        launch_model: true,
        models: &[
            "fable",
            "opus",
            "sonnet",
            "haiku",
            "fable[1m]",
            "opus[1m]",
            "sonnet[1m]",
            "haiku[1m]",
        ],
        default_model: None,
        publish_empty: false,
    }
}

fn grok_sheet() -> StaticSheet {
    StaticSheet {
        model_flag: Some("--model"),
        effort_flag: Some("--reasoning-effort"),
        launch_model: false,
        models: &["grok-4.6"],
        default_model: Some("grok-4.6"),
        publish_empty: false,
    }
}

fn cursor_sheet() -> StaticSheet {
    StaticSheet {
        model_flag: Some("--model"),
        effort_flag: None,
        launch_model: false,
        models: &[],
        default_model: None,
        publish_empty: true,
    }
}

fn flagged(model_flag: &'static str, effort_flag: Option<&'static str>) -> StaticSheet {
    StaticSheet {
        model_flag: Some(model_flag),
        effort_flag,
        launch_model: false,
        models: &[],
        default_model: None,
        publish_empty: false,
    }
}

fn flagged_empty() -> StaticSheet {
    StaticSheet {
        model_flag: None,
        effort_flag: None,
        launch_model: false,
        models: &[],
        default_model: None,
        publish_empty: false,
    }
}

pub(crate) fn sheet_inputs(config: &Config) -> BTreeMap<HarnessId, SheetInput> {
    let Some(tasks) = &config.tasks else {
        return BTreeMap::new();
    };
    let Some(harnesses) = &tasks.harnesses else {
        return BTreeMap::new();
    };
    harnesses
        .iter()
        .filter_map(|(slot, entry)| match slot {
            config::HarnessSlot::Known(harness) => Some((
                *harness,
                SheetInput {
                    models: entry.models.clone(),
                    efforts: entry.efforts.clone(),
                    mode: entry.models_mode,
                },
            )),
            config::HarnessSlot::Unknown(_) => None,
        })
        .collect()
}
