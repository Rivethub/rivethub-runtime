use agent_client_protocol::schema::v1 as acp;
use kernel::harness::SessionOption;
use kernel::harness::SessionSwitches;
use parking_lot::Mutex;
use wire::gateway::agents::model_id;

use crate::error::AcpError;

#[derive(Debug)]
pub(crate) struct ConfigSelection {
    pub(crate) config_id: acp::SessionConfigId,
    pub(crate) value: acp::SessionConfigOptionValue,
}

#[derive(Debug, Default)]
pub(crate) struct SessionOptions {
    options: Mutex<Vec<acp::SessionConfigOption>>,
}

impl SessionOptions {
    pub(crate) fn replace(&self, options: Vec<acp::SessionConfigOption>) {
        *self.options.lock() = options;
    }

    pub(crate) fn models(
        &self,
    ) -> Result<Option<Vec<kernel::harness::HarnessModelOption>>, AcpError> {
        let options = self.options.lock();
        let Some(model) = options.iter().find(|option| {
            option.category.as_ref() == Some(&acp::SessionConfigOptionCategory::Model)
        }) else {
            return Ok(None);
        };
        let acp::SessionConfigKind::Select(select) = &model.kind else {
            return Err(AcpError::ConfigOptionUnavailable { category: "model" });
        };
        let items = select_options(&select.options)?;
        if !items.iter().any(|item| item.value == select.current_value) {
            return Err(AcpError::ConfigValueRejected {
                category: "model",
                value: select.current_value.0.to_string(),
            });
        }
        let models = items
            .into_iter()
            .map(|item| kernel::harness::HarnessModelOption {
                id: model_id(&item.value.0).into_owned(),
                label: item.name.clone(),
                is_default: Some(item.value == select.current_value),
                input_modalities: None,
                efforts: None,
            })
            .collect();
        drop(options);
        Ok(Some(models))
    }

    pub(crate) fn select(&self, option: &SessionOption) -> Result<ConfigSelection, AcpError> {
        let options = self.options.lock();
        select(&options, option)
    }

    pub(crate) fn switches(&self) -> SessionSwitches {
        let options = self.options.lock();
        let offers = |category: acp::SessionConfigOptionCategory| {
            options.iter().any(|option| {
                option.category.as_ref() == Some(&category)
                    && matches!(option.kind, acp::SessionConfigKind::Select(_))
            })
        };
        SessionSwitches {
            model: offers(acp::SessionConfigOptionCategory::Model),
            effort: offers(acp::SessionConfigOptionCategory::ThoughtLevel),
        }
    }
}

pub(crate) fn stored_options(
    options: Option<Vec<acp::SessionConfigOption>>,
) -> Vec<acp::SessionConfigOption> {
    let mut stored = Vec::new();
    if let Some(options) = options {
        stored.extend(options);
    }
    stored
}

fn select(
    options: &[acp::SessionConfigOption],
    option: &SessionOption,
) -> Result<ConfigSelection, AcpError> {
    let (category, value, label) = match option {
        SessionOption::Mode(mode) => (acp::SessionConfigOptionCategory::Mode, mode.clone(), "mode"),
        SessionOption::Model(model) => (
            acp::SessionConfigOptionCategory::Model,
            model.clone(),
            "model",
        ),
        SessionOption::Effort(level) => (
            acp::SessionConfigOptionCategory::ThoughtLevel,
            level.to_string(),
            "thought_level",
        ),
    };
    let found = options
        .iter()
        .find(|item| item.category.as_ref() == Some(&category));
    let Some(found) = found else {
        return Err(AcpError::ConfigOptionUnavailable { category: label });
    };
    let offered = select_ids(found, label)?;
    let chosen = if let Some(id) = offered
        .iter()
        .find(|id| id.0.as_ref() == value || model_id(&id.0) == value.as_str())
    {
        Some(id.0.to_string())
    } else if category == acp::SessionConfigOptionCategory::Model {
        alias(&offered, &value)
    } else {
        None
    };
    let Some(chosen) = chosen else {
        return Err(AcpError::ConfigValueRejected {
            category: label,
            value,
        });
    };
    Ok(ConfigSelection {
        config_id: found.id.clone(),
        value: acp::SessionConfigOptionValue::value_id(chosen),
    })
}

fn alias(offered: &[acp::SessionConfigValueId], value: &str) -> Option<String> {
    let wanted = value.to_lowercase();
    let mut named = offered.iter().filter(|id| {
        id.0.to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|part| part == wanted)
    });
    let found = named.next()?;
    named.next().is_none().then(|| found.0.to_string())
}

fn select_ids(
    option: &acp::SessionConfigOption,
    category: &'static str,
) -> Result<Vec<acp::SessionConfigValueId>, AcpError> {
    match &option.kind {
        acp::SessionConfigKind::Select(select) => Ok(select_options(&select.options)?
            .into_iter()
            .map(|item| item.value.clone())
            .collect()),
        acp::SessionConfigKind::Boolean(_) | _ => {
            Err(AcpError::ConfigOptionUnavailable { category })
        }
    }
}

fn select_options(
    options: &acp::SessionConfigSelectOptions,
) -> Result<Vec<&acp::SessionConfigSelectOption>, AcpError> {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(items) => Ok(items.iter().collect()),
        acp::SessionConfigSelectOptions::Grouped(groups) => {
            Ok(groups.iter().flat_map(|group| &group.options).collect())
        }
        _ => Err(AcpError::ConfigOptionUnavailable { category: "select" }),
    }
}

#[cfg(test)]
mod tests {
    use super::select;
    use agent_client_protocol::schema::v1 as acp;
    use kernel::harness::SessionOption;
    use wire::stream::ThinkingLevel;

    use crate::error::AcpError;

    fn model_option(current: &str) -> acp::SessionConfigOption {
        acp::SessionConfigOption::select(
            "model",
            "Model",
            current.to_owned(),
            vec![
                acp::SessionConfigSelectOption::new("grok-5", "grok-5"),
                acp::SessionConfigSelectOption::new("grok-4.7", "grok-4.7"),
            ],
        )
        .category(acp::SessionConfigOptionCategory::Model)
    }

    fn effort_option() -> acp::SessionConfigOption {
        acp::SessionConfigOption::select(
            "reasoning_effort",
            "Reasoning effort",
            "high",
            vec![acp::SessionConfigSelectGroup::new(
                "levels",
                "Levels",
                vec![
                    acp::SessionConfigSelectOption::new("low", "low"),
                    acp::SessionConfigSelectOption::new("high", "high"),
                ],
            )],
        )
        .category(acp::SessionConfigOptionCategory::ThoughtLevel)
    }

    #[test]
    fn model_and_grouped_effort_resolve_to_advertised_ids() {
        let options = vec![model_option("grok-4.7"), effort_option()];
        let model = select(&options, &SessionOption::Model("grok-5".to_owned())).expect("model");
        assert_eq!(model.config_id.0.as_ref(), "model");
        assert_eq!(
            model.value,
            acp::SessionConfigOptionValue::value_id("grok-5")
        );
        let effort = select(&options, &SessionOption::Effort(ThinkingLevel::Low)).expect("effort");
        assert_eq!(effort.config_id.0.as_ref(), "reasoning_effort");
        assert_eq!(effort.value, acp::SessionConfigOptionValue::value_id("low"));
    }

    #[test]
    fn a_model_alias_resolves_to_the_one_advertised_id_that_names_it() {
        let ids = |ids: &[&str]| {
            acp::SessionConfigOption::select(
                "model",
                "Model",
                ids.first().copied().unwrap_or_default().to_owned(),
                ids.iter()
                    .map(|id| {
                        acp::SessionConfigSelectOption::new((*id).to_owned(), (*id).to_owned())
                    })
                    .collect::<Vec<_>>(),
            )
            .category(acp::SessionConfigOptionCategory::Model)
        };
        let claude = vec![ids(&["default", "opus", "claude-fable-5-1[1m]", "sonnet"])];
        let fable = select(&claude, &SessionOption::Model("fable".to_owned())).expect("alias");
        assert_eq!(
            fable.value,
            acp::SessionConfigOptionValue::value_id("claude-fable-5-1[1m]")
        );
        let twins = vec![ids(&["claude-fable-5", "claude-fable-5-1"])];
        assert!(matches!(
            select(&twins, &SessionOption::Model("fable".to_owned())),
            Err(AcpError::ConfigValueRejected { .. })
        ));
    }

    #[test]
    fn a_provider_and_model_pair_is_offered_and_chosen_as_provider_slash_model() {
        let value = r#"["gpubox","deepseek-v4.1-flash"]"#;
        let options = vec![
            acp::SessionConfigOption::select(
                "model",
                "Model",
                value.to_owned(),
                vec![acp::SessionConfigSelectOption::new(
                    value.to_owned(),
                    "Flash",
                )],
            )
            .category(acp::SessionConfigOptionCategory::Model),
        ];
        let stored = super::SessionOptions::default();
        stored.replace(options.clone());
        let models = stored.models().expect("models").expect("offered");
        assert_eq!(
            models.first().expect("model").id,
            "gpubox/deepseek-v4.1-flash"
        );
        for asked in ["gpubox/deepseek-v4.1-flash", value] {
            let chosen = select(&options, &SessionOption::Model(asked.to_owned())).expect(asked);
            assert_eq!(chosen.value, acp::SessionConfigOptionValue::value_id(value));
        }
    }

    #[test]
    fn a_missing_category_and_a_rejected_value_fail_closed() {
        let options = vec![model_option("grok-4.7")];
        let missing = select(&options, &SessionOption::Effort(ThinkingLevel::High))
            .expect_err("no thought level");
        assert!(matches!(
            missing,
            AcpError::ConfigOptionUnavailable {
                category: "thought_level"
            }
        ));
        let rejected = select(&options, &SessionOption::Model("missing".to_owned()))
            .expect_err("value not offered");
        assert!(matches!(
            rejected,
            AcpError::ConfigValueRejected {
                category: "model",
                ..
            }
        ));
    }

    #[test]
    fn permission_modes_require_an_advertised_mode_and_value() {
        let options = vec![
            acp::SessionConfigOption::select(
                "permission_mode",
                "Permissions",
                "plan",
                vec![acp::SessionConfigSelectOption::new("plan", "Plan")],
            )
            .category(acp::SessionConfigOptionCategory::Mode),
        ];
        let selected = select(&options, &SessionOption::Mode("plan".into())).expect("mode");
        assert_eq!(selected.config_id.0.as_ref(), "permission_mode");
        assert_eq!(
            selected.value,
            acp::SessionConfigOptionValue::value_id("plan")
        );
        assert!(matches!(
            select(&options, &SessionOption::Mode("missing".into())),
            Err(AcpError::ConfigValueRejected {
                category: "mode",
                ..
            })
        ));
        assert!(matches!(
            select(&[], &SessionOption::Mode("plan".into())),
            Err(AcpError::ConfigOptionUnavailable { category: "mode" })
        ));
    }

    #[test]
    fn a_boolean_option_is_not_a_model_selector() {
        let options = vec![
            acp::SessionConfigOption::boolean("model", "Model", true)
                .category(acp::SessionConfigOptionCategory::Model),
        ];
        let error = select(&options, &SessionOption::Model("grok-5".to_owned()))
            .expect_err("boolean is not selectable");
        assert!(matches!(
            error,
            AcpError::ConfigOptionUnavailable { category: "model" }
        ));
    }
}
