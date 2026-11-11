use config::Environment;
use regress::Regex;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::json::Scrub;

use crate::rules::Ruleset;

pub const ENV_VARIABLE: &str = "RIVETHUB_CAPTURE_REDACTION";
const SCAN_LIMIT: usize = 16_000;
const SCAN_MARGIN: usize = 8_192;
pub(crate) const SECRET_KEY_PLACEHOLDER: &str = "[REDACTED:secret_key]";

#[derive(Debug)]
pub struct Redaction {
    rules: Option<&'static Ruleset>,
    patterns: Vec<Regex>,
}

#[must_use]
pub fn resolve(
    settings: Option<config::RedactionSettings>,
    env: &Environment,
) -> Option<Redaction> {
    let (builtins_on, patterns) = match settings {
        Some(settings) if settings.enabled => (settings.builtins, settings.patterns),
        _ => {
            if !env_opt_in(env) {
                return None;
            }
            (true, Vec::new())
        }
    };
    if !builtins_on && patterns.is_empty() {
        return None;
    }
    let mut compiled = Vec::new();
    for (index, source) in patterns.iter().enumerate() {
        if unsafe_source(source) {
            tracing::warn!("capture redaction pattern {index} is a nested quantifier; skipped");
            continue;
        }
        match Regex::new(source) {
            Ok(regex) => compiled.push(regex),
            Err(error) => {
                tracing::warn!("capture redaction pattern {index} did not compile: {error}");
            }
        }
    }
    Some(Redaction {
        rules: builtins_on.then(crate::rules::ruleset),
        patterns: compiled,
    })
}

fn env_opt_in(env: &Environment) -> bool {
    env.text(ENV_VARIABLE).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl Redaction {
    pub fn redact_batch(&self, batch: &mut CaptureBatchRequest) -> u64 {
        let mut spans = 0;
        if let Some(title) = &mut batch.title {
            let (redacted, count) = apply_spans(title, self);
            *title = redacted;
            spans += count;
        }
        if let Some(settings) = &mut batch.settings {
            spans += self.redact_object(settings);
        }
        for message in &mut batch.messages {
            spans += self.redact_message(message);
        }
        spans
    }

    pub fn redact_message(&self, message: &mut CaptureMessage) -> u64 {
        let mut spans = 0;
        if let Some((redacted, count)) = scan_text(&message.content, self) {
            message.content = redacted;
            spans += count;
        }
        if let Some(result) = &message.tool_result
            && let Some((redacted, count)) = scan_text(result, self)
        {
            message.tool_result = Some(redacted);
            spans += count;
        }
        if let Some(arguments) = &mut message.tool_args {
            spans += self.redact_object(arguments);
        }
        if let Some(metadata) = &mut message.metadata {
            spans += self.redact_object(metadata);
        }
        spans
    }

    fn redact_object(&self, object: &mut JsonObject) -> u64 {
        let mut live = LiveScrub {
            redaction: self,
            spans: 0,
        };
        object.scrub(&mut live);
        live.spans
    }
}

struct LiveScrub<'a> {
    redaction: &'a Redaction,
    spans: u64,
}

impl Scrub for LiveScrub<'_> {
    fn is_secret_name(&self, key: &str) -> bool {
        self.redaction
            .rules
            .is_some_and(|rules| rules.is_secret_name(key))
    }

    fn rank(&self, _key: &str) -> u64 {
        0
    }

    fn key(&mut self, _parent: &str, key: &mut String) -> usize {
        let (redacted, count) = apply_spans(key, self.redaction);
        *key = redacted;
        self.spans += count;
        0
    }

    fn renamed(
        &mut self,
        _parent: &str,
        _markers: (usize, Option<usize>),
        _key: &str,
        _renamed: &str,
    ) {
    }

    fn text(&mut self, _path: &str, text: &mut String) {
        let (redacted, count) = apply_spans(text, self.redaction);
        *text = redacted;
        self.spans += count;
    }

    fn secret(&mut self, _path: &str, key: &str, text: &mut String) -> bool {
        let redacts = self
            .redaction
            .rules
            .is_some_and(|rules| rules.names().redacts(key, text));
        if redacts {
            SECRET_KEY_PLACEHOLDER.clone_into(text);
            self.spans += 1;
        }
        redacts
    }
}

fn apply_spans(text: &str, redaction: &Redaction) -> (String, u64) {
    let mut out = text.to_owned();
    let mut count = 0;
    if let Some(rules) = redaction.rules {
        let found = rules.spans(&out);
        let replaced: Vec<(std::ops::Range<usize>, String)> = found
            .iter()
            .map(|span| (span.range.clone(), placeholder(&span.rule.id)))
            .collect();
        count += replaced.len() as u64;
        out = replace_spans(&out, &replaced);
    }
    for (index, pattern) in redaction.patterns.iter().enumerate() {
        let spans: Vec<_> = pattern
            .find_iter(&out)
            .map(|found| (found.range(), format!("[REDACTED:pattern:{index}]")))
            .collect();
        count += spans.len() as u64;
        out = replace_spans(&out, &spans);
    }
    (out, count)
}

#[must_use]
pub fn placeholder(kind: &str) -> String {
    format!("[REDACTED:{kind}]")
}

fn scan_text(text: &str, redaction: &Redaction) -> Option<(String, u64)> {
    let mut window = SCAN_LIMIT + SCAN_MARGIN;
    loop {
        let (head, tail) = split_at_char(text, window);
        let (redacted_head, count) = apply_spans(head, redaction);
        let covered = tail.is_empty() || redacted_head.chars().count() >= SCAN_LIMIT + SCAN_MARGIN;
        if covered {
            if redacted_head == head {
                return None;
            }
            return Some((format!("{redacted_head}{tail}"), count));
        }
        window = window.saturating_mul(2);
    }
}

fn split_at_char(text: &str, chars: usize) -> (&str, &str) {
    text.char_indices()
        .nth(chars)
        .map_or((text, ""), |(end, _)| text.split_at(end))
}

fn replace_spans(text: &str, spans: &[(std::ops::Range<usize>, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (span, placeholder) in spans {
        if span.start < copied {
            continue;
        }
        out.push_str(text.get(copied..span.start).unwrap_or_default());
        out.push_str(placeholder);
        copied = span.end;
    }
    out.push_str(text.get(copied..).unwrap_or_default());
    out
}

fn unsafe_source(source: &str) -> bool {
    let characters: Vec<char> = source.chars().collect();
    let mut depth = 0;
    let mut group_start = 0;
    let mut index = 0;
    while index < characters.len() {
        match characters.get(index) {
            Some('\\') => index += 1,
            Some('(') => {
                if depth == 0 {
                    group_start = index;
                }
                depth += 1;
            }
            Some(')') if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    #[expect(
                        clippy::indexing_slicing,
                        reason = "group_start and index bracket one group inside the source"
                    )]
                    let body = &characters[group_start + 1..index];
                    let repeated = characters
                        .get(index + 1)
                        .is_some_and(|next| matches!(next, '+' | '*' | '{' | '?'));
                    if ends_quantified(body) && repeated {
                        return true;
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn ends_quantified(body: &[char]) -> bool {
    let trimmed: String = body.iter().filter(|c| !c.is_whitespace()).collect();
    trimmed.ends_with('+')
        || trimmed.ends_with('*')
        || trimmed.ends_with('?')
        || trimmed.ends_with('}')
}
