use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use super::Context;
use super::Family;
use super::Names;
use super::Verdict;
use super::value;

#[expect(
    clippy::expect_used,
    reason = "constant patterns; pair_forms_compile runs them in CI"
)]
static FORMS: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        r#"(?i)\\?"name\\?"[ \t]*:[ \t]*\\?"([^"\\\n]{1,200})\\?"[ \t]*,\s*\\?"value\\?"[ \t]*:[ \t]*"#,
        r#"(?im)^[ \t]*-[ \t]+name:[ \t]*["']?([A-Za-z_][\w.-]*)["']?[ \t]*\r?\n[ \t]+value:[ \t]*"#,
    ]
    .map(|source| Regex::new(source).expect("pair forms compile"))
});

impl Names {
    pub(in crate::rules) fn pair_hits(&self, text: &str) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        for form in FORMS.iter() {
            let mut lines = value::Lines::default();
            let mut redacted_to = 0;
            let mut kept = value::KeptBudget::for_text(text);
            for captures in form.captures_iter(text) {
                let (Some(whole), Some(name)) = (captures.get(0), captures.get(1)) else {
                    continue;
                };
                if whole.start() < redacted_to {
                    continue;
                }
                if self.family(name.as_str()).is_none() {
                    continue;
                }
                let rest = text.get(whole.end()..).unwrap_or_default();
                let line_end = lines.end(text, whole.end()) - whole.end();
                let Some(parsed) = value::parse(rest, line_end, &|text| self.placeholder_len(text))
                else {
                    continue;
                };
                let Some(slice) = rest.get(parsed.range.clone()) else {
                    continue;
                };
                let ctx = Context {
                    key: name.as_str(),
                    ..Context::default()
                };
                let redact = kept.exhausted()
                    || matches!(
                        self.judge(Family::Password, slice, parsed.shape, ctx),
                        Verdict::Redact
                    );
                if redact {
                    redacted_to = whole.end() + parsed.range.end;
                    found.push(whole.end() + parsed.range.start..redacted_to);
                } else {
                    kept.spend(parsed.range.end);
                }
            }
        }
        found.sort_by_key(|range| range.start);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_forms_compile() {
        assert_eq!(FORMS.len(), 2);
    }
}
