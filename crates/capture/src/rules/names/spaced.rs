use std::ops::Range;

use super::Context;
use super::Family;
use super::Names;
use super::Shape;
use super::Verdict;
use super::value;

impl Names {
    pub(super) fn spaced_hits(&self, text: &str, found: &[Range<usize>]) -> Vec<Range<usize>> {
        let mut spaced = Vec::new();
        for captures in self.forms.spaced.captures_iter(text) {
            let (Some(whole), Some(word)) = (captures.get(0), captures.get(1)) else {
                continue;
            };
            let before = found.partition_point(|range| range.start <= word.start());
            if before
                .checked_sub(1)
                .and_then(|index| found.get(index))
                .is_some_and(|range| word.start() < range.end)
            {
                continue;
            }
            let Some(rest) = text.get(whole.end()..) else {
                continue;
            };
            if rest.starts_with(|c: char| c.is_whitespace() || c == '=' || c == ':') {
                continue;
            }
            let Some(end) = value::token_end(rest) else {
                continue;
            };
            let Some(value) = rest.get(..end) else {
                continue;
            };
            if self.forms.word.is_match(value) {
                continue;
            }
            let followed_by_separator = rest
                .get(end..)
                .is_some_and(|tail| tail.trim_start().starts_with(['=', ':']));
            if followed_by_separator {
                continue;
            }
            let ctx = Context {
                key: word.as_str(),
                ..Context::default()
            };
            if matches!(
                self.judge(Family::Token, value, Shape::Bare, ctx),
                Verdict::Redact
            ) {
                spaced.push(whole.end()..whole.end() + end);
            }
        }
        spaced
    }
}
