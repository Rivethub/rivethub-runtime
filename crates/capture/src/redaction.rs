use std::sync::LazyLock;

use config::Environment;
use regress::Regex;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;

pub const ENV_VARIABLE: &str = "RIVETHUB_CAPTURE_REDACTION";
const SCAN_LIMIT: usize = 16_000;
const SCAN_MARGIN: usize = 8_192;
const SECRET_KEY_SOURCE: &str = "^(?:pwd|pass|auth)$|passw(?:or)?d|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credential|authorization|auth[_-]?header|cookie|session[_-]?key";
pub(crate) const SECRET_KEY_PLACEHOLDER: &str = "[REDACTED:secret_key]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replacement {
    Whole,
    FirstGroup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Insensitive,
    Sensitive,
    SensitiveLines,
}

impl Case {
    const fn flags(self) -> &'static str {
        match self {
            Self::Insensitive => "i",
            Self::Sensitive => "",
            Self::SensitiveLines => "m",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Anywhere,
    LineWith(&'static str),
}

#[derive(Debug)]
pub(crate) struct Builtin {
    pub(crate) name: &'static str,
    pub(crate) source: &'static str,
    regex: Regex,
    replacement: Replacement,
    line_needle: Option<regex::Regex>,
}

impl Builtin {
    pub(crate) fn spans(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        let Some(needle) = &self.line_needle else {
            return self.spans_in(text, 0);
        };
        let mut spans = Vec::new();
        let mut scanned = 0;
        for found in needle.find_iter(text) {
            if found.start() < scanned {
                continue;
            }
            let start = text[..found.start()].rfind('\n').map_or(0, |at| at + 1);
            let end = text[found.end()..]
                .find('\n')
                .map_or(text.len(), |at| found.end() + at);
            spans.extend(self.spans_in(&text[start..end], start));
            scanned = end;
        }
        spans
    }

    fn spans_in(&self, text: &str, offset: usize) -> Vec<std::ops::Range<usize>> {
        self.regex
            .find_iter(text)
            .map(|found| match self.replacement {
                Replacement::Whole => found.range(),
                Replacement::FirstGroup => {
                    let kept = found
                        .group(1)
                        .map_or_else(|| found.start(), |group| group.end);
                    kept..found.end()
                }
            })
            .map(|span| span.start + offset..span.end + offset)
            .collect()
    }

    pub(crate) fn placeholder(&self) -> String {
        format!("[REDACTED:{}]", self.name)
    }
}

const BUILTIN_SOURCES: [(&str, &str, Replacement, Case, Scope); 11] = [
    (
        "bearer",
        r"Bearer [A-Za-z0-9\-._~+/=]{16,}",
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "pem_private_key",
        concat!(
            "-----BEGIN [A-Z ]*PRIVATE ",
            r"KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE ",
            "KEY-----"
        ),
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "aws_access_key",
        r"AKIA[0-9A-Z]{16}",
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "github_token",
        r"gh[pousr]_[A-Za-z0-9]{30,}",
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "slack_token",
        r"xox[aebprs]-[A-Za-z0-9\-]{10,}",
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "sk_token",
        concat!("sk", r"[_\-][A-Za-z0-9_\-]{20,}"),
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "jwt",
        r"ey[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
        Replacement::Whole,
        Case::Insensitive,
        Scope::Anywhere,
    ),
    (
        "http_auth",
        r"\b(?:Basic|Digest|Negotiate|NTLM) (?=[A-Za-z0-9+/=._~\-]*[A-Z0-9+/=])[A-Za-z0-9+/=._~\-]{8,}",
        Replacement::Whole,
        Case::Sensitive,
        Scope::LineWith("Basic |Digest |Negotiate |NTLM "),
    ),
    (
        "assignment",
        r#"\b(api[_-]?key|apikey|access[_-]?key|access[_-]?token|auth[_-]?token|secret|token|authorization|credential)["']?[ \t]*[=:][ \t]*\S+"#,
        Replacement::FirstGroup,
        Case::Insensitive,
        Scope::LineWith("(?i)key|token|secret|authorization|credential"),
    ),
    (
        "env_secret",
        r#"^([ \t]*(?:export[ \t]+)?[A-Z0-9_]*(?:PASSWORD|PASSWD|SECRET|TOKEN|API_KEY|APIKEY|PRIVATE_KEY|ACCESS_KEY|CREDENTIALS?)[A-Z0-9_]*[ \t]*=[ \t]*)(?:"[^"\n]*"|'[^'\n]*'|[^\s"']+)"#,
        Replacement::FirstGroup,
        Case::SensitiveLines,
        Scope::LineWith(
            "PASSWORD|PASSWD|SECRET|TOKEN|API_KEY|APIKEY|PRIVATE_KEY|ACCESS_KEY|CREDENTIAL",
        ),
    ),
    (
        "url_password",
        r"([a-z][a-z0-9+.\-]*://[^\s:/@\[\]]+:)[^\s@/\[\]]+(?=@)",
        Replacement::FirstGroup,
        Case::Insensitive,
        Scope::LineWith("://"),
    ),
];

#[expect(
    clippy::expect_used,
    reason = "the builtin sources are constants; the redaction tests compile every one of them"
)]
static SECRET_KEYS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(SECRET_KEY_SOURCE, "i")
        .expect("the secret-key pattern is a constant that compiles")
});

#[expect(
    clippy::expect_used,
    reason = "the builtin sources are constants; the redaction tests compile every one of them"
)]
static BUILTINS: LazyLock<Vec<Builtin>> = LazyLock::new(|| {
    BUILTIN_SOURCES
        .into_iter()
        .map(|(name, source, replacement, case, scope)| Builtin {
            name,
            source,
            regex: Regex::with_flags(source, case.flags())
                .expect("builtin redaction patterns are constants that compile"),
            replacement,
            line_needle: match scope {
                Scope::Anywhere => None,
                Scope::LineWith(needle) => Some(
                    regex::Regex::new(needle)
                        .expect("builtin line needles are constants that compile"),
                ),
            },
        })
        .collect()
});

#[derive(Debug)]
pub struct Redaction {
    builtins: &'static [Builtin],
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
    let builtins: &'static [Builtin] = if builtins_on { &BUILTINS } else { &[] };
    Some(Redaction {
        builtins,
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
        let mut spans = 0;
        object.visit_strings_mut(&mut |name, text| {
            let (redacted, count) = apply_spans(text, self);
            spans += count;
            if secret_key(name) {
                SECRET_KEY_PLACEHOLDER.clone_into(text);
            } else {
                *text = redacted;
            }
        });
        spans
    }
}

fn apply_spans(text: &str, redaction: &Redaction) -> (String, u64) {
    let mut out = text.to_owned();
    let mut count = 0;
    for builtin in redaction.builtins {
        let (next, hits) = replace_spans(&out, &builtin.spans(&out), &builtin.placeholder());
        count += hits;
        out = next;
    }
    for (index, pattern) in redaction.patterns.iter().enumerate() {
        let spans: Vec<_> = pattern.find_iter(&out).map(|found| found.range()).collect();
        let (next, hits) = replace_spans(&out, &spans, &format!("[REDACTED:pattern:{index}]"));
        count += hits;
        out = next;
    }
    (out, count)
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

pub(crate) fn builtins() -> &'static [Builtin] {
    &BUILTINS
}

pub(crate) const fn secret_key_source() -> &'static str {
    SECRET_KEY_SOURCE
}

pub(crate) fn secret_key(key: &str) -> bool {
    SECRET_KEYS
        .find(key)
        .is_some_and(|found| !found.range().is_empty())
}

fn replace_spans(text: &str, spans: &[std::ops::Range<usize>], placeholder: &str) -> (String, u64) {
    let mut out = String::new();
    let mut copied = 0;
    for span in spans {
        out.push_str(&text[copied..span.start]);
        out.push_str(placeholder);
        copied = span.end;
    }
    out.push_str(&text[copied..]);
    (out, spans.len() as u64)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns_are_constants_that_compile() {
        assert_eq!(BUILTINS.len(), BUILTIN_SOURCES.len());
        let canaries = [
            ("bearer", "Bearer abcdefghijklmnop1234"),
            (
                "pem_private_key",
                concat!(
                    "-----BEGIN ",
                    "PRIVATE KEY-----\nx\n-----END ",
                    "PRIVATE KEY-----"
                ),
            ),
            ("aws_access_key", concat!("AKIA", "ABCDEFGHIJKLMNOP")),
            (
                "github_token",
                concat!("ghp_", "abcdefghijklmnopqrstuvwx0123456789"),
            ),
            ("slack_token", concat!("xoxb-", "abcdefghij")),
            ("sk_token", concat!("sk-", "abcdefghijklmnopqrst")),
            (
                "jwt",
                concat!(
                    "eyJhbGciOiJIUzI1NiJ9",
                    ".eyJzdWIiOiIxIn0",
                    ".SflKxwRJSMeKKF2QT4"
                ),
            ),
            ("http_auth", concat!("Basic ", "dXNlcjpwYXNz")),
            ("assignment", "api_key = hunter2"),
            ("env_secret", "DB_PASSWORD=hunter2"),
            ("url_password", "postgres://app:hunter2@db.example.com/app"),
        ];
        for (name, canary) in canaries {
            let builtin = BUILTINS
                .iter()
                .find(|builtin| builtin.name == name)
                .unwrap_or_else(|| panic!("builtin {name} is missing"));
            assert!(
                builtin.regex.find(canary).is_some(),
                "builtin {name} does not match its canary"
            );
        }
        assert!(
            SECRET_KEYS.find("api_key").is_some(),
            "the secret-key pattern is missing"
        );
    }

    #[test]
    fn scanning_only_needle_lines_finds_what_a_whole_text_scan_finds() {
        let text = concat!(
            "intro line\nAuthorization: Basic dXNlcjpwYXNz and Digest abcdefgh12\n",
            "  export DB_PASSWORD=\"two words\"\nAPI_KEY=x\nnot_a_key here\n",
            "token: abc token=def\r\nsecret\n= split\ncredential:\tzz key",
            "\npostgres://app:pw@db.example.com/app https://a:b@c x://no-at:here\n",
            "NTLM short\nBearer abcdefghijklmnop1234\n:// \nend"
        );
        for builtin in BUILTINS.iter() {
            assert_eq!(
                builtin.spans(text),
                builtin.spans_in(text, 0),
                "builtin {}",
                builtin.name
            );
        }
    }

    #[test]
    fn a_capture_group_that_does_not_participate_keeps_nothing() {
        let source = "x(?:-(y))?";
        let builtin = Builtin {
            name: "test",
            source,
            regex: Regex::new(source).expect("the test pattern is a constant that compiles"),
            replacement: Replacement::FirstGroup,
            line_needle: None,
        };
        assert_eq!(builtin.spans("a x b"), vec![2..3]);
        assert_eq!(builtin.spans("a x-y b"), vec![5..5]);
    }
}
