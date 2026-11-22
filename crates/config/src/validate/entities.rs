use crate::model::EntitiesSection;
use crate::report::ValidationResult;

const REWRITE_CONCURRENCY_MAX: u32 = 64;

pub(super) fn validate(out: &mut ValidationResult, section: &EntitiesSection) {
    let matcher = trimmed(section.matcher.as_deref());
    if let Some(matcher) = matcher
        && matcher != "thinkingcap"
        && matcher != "jev"
    {
        out.error(
            "entities.matcher",
            format!("entities.matcher must be thinkingcap or jev, not {matcher}"),
        );
    }
    match trimmed(section.matcher_url.as_deref()) {
        Some(text) if !web_url(text) => {
            out.error(
                "entities.matcher_url",
                "The entities matcher url must be http or https",
            );
        }
        None if matcher == Some("jev") => {
            out.error(
                "entities.matcher_url",
                "entities.matcher jev needs matcher_url",
            );
        }
        _ => {}
    }
    if let Some(value) = section.rewrite_concurrency
        && !(1..=REWRITE_CONCURRENCY_MAX).contains(&value)
    {
        out.error(
            "entities.rewrite_concurrency",
            format!(
                "entities.rewrite_concurrency must be between 1 and {REWRITE_CONCURRENCY_MAX}, not {value}"
            ),
        );
    }
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn web_url(text: &str) -> bool {
    url::Url::parse(text)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
}
