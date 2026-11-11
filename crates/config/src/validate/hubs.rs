use std::collections::BTreeSet;

use crate::model::{CloudSection, HubEntry};
use crate::report::ValidationResult;

pub(super) fn validate(out: &mut ValidationResult, hubs: &[HubEntry]) {
    let mut names = BTreeSet::new();
    for (index, hub) in hubs.iter().enumerate() {
        let path = format!("hubs.{index}");
        if hub.name.trim().is_empty() {
            out.error(format!("{path}.name"), "A hub needs a name");
        } else if !names.insert(hub.name.trim().to_lowercase()) {
            out.error(
                format!("{path}.name"),
                format!("Hub name \"{}\" is listed twice", hub.name),
            );
        }
        if !web_url(&hub.url) {
            out.error(format!("{path}.url"), "A hub url must be http or https");
        }
    }
}

pub(super) fn cloud(out: &mut ValidationResult, section: &CloudSection) {
    if section.url.as_ref().is_some_and(|url| !web_url(url)) {
        out.error("cloud.url", "The cloud url must be http or https");
    }
}

fn web_url(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host().is_some()
}
