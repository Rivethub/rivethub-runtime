use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {detail}")]
    Slice { path: PathBuf, detail: String },
    #[error("label {label} must match [A-Za-z0-9._-]+")]
    Label { label: String },
    #[error("concurrency must be at least 1")]
    Concurrency,
    #[error("{detail}")]
    Usage { detail: String },
    #[error("runtime: {0}")]
    Runtime(std::io::Error),
    #[error("database: {0}")]
    Database(String),
    #[error("migrate: {0}")]
    Migrate(String),
    #[error("model server is unreachable: {0}")]
    Model(String),
    #[error("copy: {0}")]
    Copy(String),
    #[error("the slice copied 0 conversations")]
    EmptyCopy,
    #[error("copied {copied} conversations, the slice names {expected}")]
    PartialCopy { copied: i64, expected: usize },
    #[error("queue: {0}")]
    Queue(String),
    #[error("worker: {0}")]
    Worker(String),
    #[error("report: {0}")]
    Report(String),
    #[error("owner: {0}")]
    Owner(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("{path} already exists")]
    Exists { path: PathBuf },
}

pub(crate) fn secrets(urls: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for raw in urls {
        found.push((*raw).to_owned());
        let Ok(parsed) = url::Url::parse(raw) else {
            continue;
        };
        if let Some(password) = parsed.password()
            && !password.is_empty()
        {
            found.push(password.to_owned());
            let decoded = percent_decode(password);
            if decoded != password {
                found.push(decoded);
            }
        }
    }
    found
}

pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        if secret.len() >= 4 {
            out = out.replace(secret, "[redacted]");
        }
    }
    scrub_urls(&out)
}

pub(crate) fn database(error: &impl std::fmt::Display, secrets: &[String]) -> Failure {
    Failure::Database(redact(&error.to_string(), secrets))
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let Some(current) = bytes.get(index).copied() else {
            break;
        };
        if current == b'%'
            && let Some(value) = hex_byte(
                bytes.get(index.saturating_add(1)).copied(),
                bytes.get(index.saturating_add(2)).copied(),
            )
        {
            out.push(value);
            index = index.saturating_add(3);
            continue;
        }
        out.push(current);
        index = index.saturating_add(1);
    }
    String::from_utf8(out).unwrap_or_else(|_| raw.to_owned())
}

fn hex_byte(hi: Option<u8>, lo: Option<u8>) -> Option<u8> {
    let hi = from_hex(hi?)?;
    let lo = from_hex(lo?)?;
    Some(hi.saturating_mul(16).saturating_add(lo))
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(byte.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

fn scrub_urls(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let start = rest
            .find("postgres://")
            .or_else(|| rest.find("postgresql://"));
        let Some(start) = start else {
            out.push_str(rest);
            return out;
        };
        let (head, tail) = rest.split_at(start);
        out.push_str(head);
        out.push_str("postgres://[redacted]");
        let skip = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (_, next) = tail.split_at(skip);
        rest = next;
    }
}

#[expect(
    clippy::let_underscore_must_use,
    reason = "a closed stderr cannot carry the progress line anywhere else"
)]
pub(crate) fn note(line: &str) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "wiki-eval: {line}");
}

#[expect(
    clippy::let_underscore_must_use,
    reason = "the failure report is the process's last write"
)]
#[expect(
    clippy::let_underscore_untyped,
    reason = "the failure report is the process's last write"
)]
pub(crate) fn report(error: &Failure) -> std::process::ExitCode {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "wiki-eval: {error}");
    let _ = stderr.flush();
    std::process::ExitCode::FAILURE
}
