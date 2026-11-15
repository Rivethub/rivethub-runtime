use std::collections::HashMap;
use std::sync::LazyLock;

use parking_lot::Mutex;
use regex::Regex;

#[rustfmt::skip]
pub type Json = serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Masks {
    pub port: String,
    pub hostname: Option<String>,
    pub tmp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenClass {
    Date,
    Timestamp,
    Port,
    Tmp,
    Hostname,
    EpochMs,
    Repo,
    Node,
    NodeVersion,
    Version,
    Revision,
    Key,
    GenId,
    Pty { id: String },
    Pid { id: String },
}

impl TokenClass {
    const DATE_PATTERN: &'static str =
        "[A-Za-z]{3}, [0-9]{2} [A-Za-z]{3} [0-9]{4} [0-9]{2}:[0-9]{2}:[0-9]{2} GMT";
    const TIMESTAMP_PATTERN: &'static str = "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:[.][0-9]+)?(?:Z|[+-][0-9]{2}:[0-9]{2})";
    const DIGITS_PATTERN: &'static str = "[0-9]+";
    const TMP_PATTERN: &'static str = r#"[^ "\\)\]]{1,1024}"#;
    const HOSTNAME_PATTERN: &'static str = r#"[^"\\]{1,256}"#;
    const EPOCH_MS_PATTERN: &'static str = "[0-9]{13}";
    const PATHISH_PATTERN: &'static str = r#"[^"\\]{1,1024}"#;
    const NODE_VERSION_PATTERN: &'static str = "v?[0-9]+[.][0-9]+[.][0-9]+";
    const VERSION_PATTERN: &'static str = "[0-9]+[.][0-9]+[.][0-9]+(?:-[0-9A-Za-z.-]+)?";
    const REVISION_PATTERN: &'static str = "[0-9a-f]{7,40}";
    const KEY_PATTERN: &'static str = "[0-9]{13}-[0-9a-z]+";
    const GENID_PATTERN: &'static str = "(?:aitxt-[0-9A-Za-z]{24,}|[0-9A-Za-z]{16,})";
    const PTY_PATTERN: &'static str = "pty-[0-9a-f]{8}";

    fn named(name: &str) -> Option<Self> {
        match name {
            "DATE" => Some(Self::Date),
            "TS" | "GENTS" => Some(Self::Timestamp),
            "PORT" => Some(Self::Port),
            "TMP" => Some(Self::Tmp),
            "HOSTNAME" => Some(Self::Hostname),
            "EPOCH_MS" => Some(Self::EpochMs),
            "REPO" => Some(Self::Repo),
            "NODE" => Some(Self::Node),
            "NODE_VERSION" => Some(Self::NodeVersion),
            "VERSION" => Some(Self::Version),
            "REVISION" => Some(Self::Revision),
            "KEY" => Some(Self::Key),
            "GENID" => Some(Self::GenId),
            _ => None,
        }
    }

    fn numbered(name: &str) -> Option<Self> {
        let (prefix, number) = name.split_once('_')?;
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let id = number.to_owned();
        match prefix {
            "PTY" => Some(Self::Pty { id }),
            "PID" => Some(Self::Pid { id }),
            _ => None,
        }
    }

    fn pattern(&self) -> &'static str {
        match self {
            Self::Date => Self::DATE_PATTERN,
            Self::Timestamp => Self::TIMESTAMP_PATTERN,
            Self::Port | Self::Pid { .. } => Self::DIGITS_PATTERN,
            Self::Tmp => Self::TMP_PATTERN,
            Self::Hostname => Self::HOSTNAME_PATTERN,
            Self::EpochMs => Self::EPOCH_MS_PATTERN,
            Self::Repo | Self::Node => Self::PATHISH_PATTERN,
            Self::NodeVersion => Self::NODE_VERSION_PATTERN,
            Self::Version => Self::VERSION_PATTERN,
            Self::Revision => Self::REVISION_PATTERN,
            Self::Key => Self::KEY_PATTERN,
            Self::GenId => Self::GENID_PATTERN,
            Self::Pty { .. } => Self::PTY_PATTERN,
        }
    }

    fn concrete(&self, masks: &Masks) -> Option<String> {
        match self {
            Self::Port => Some(masks.port.clone()),
            Self::Tmp => masks.tmp.clone(),
            Self::Hostname => masks.hostname.clone(),
            Self::Date
            | Self::Timestamp
            | Self::EpochMs
            | Self::Repo
            | Self::Node
            | Self::NodeVersion
            | Self::Version
            | Self::Revision
            | Self::Key
            | Self::GenId
            | Self::Pty { .. }
            | Self::Pid { .. } => None,
        }
    }
}

static PATTERN_CACHE: LazyLock<Mutex<HashMap<String, Regex>>> = LazyLock::new(Mutex::default);

fn compiled(source: &str) -> Result<Regex, regex::Error> {
    if let Some(built) = PATTERN_CACHE.lock().get(source) {
        return Ok(built.clone());
    }
    let built = Regex::new(source)?;
    PATTERN_CACHE
        .lock()
        .insert(source.to_owned(), built.clone());
    Ok(built)
}

struct Masked {
    source: String,
    numbered: Vec<(TokenClass, String)>,
}

fn masked_pattern(rec: &str, masks: &Masks) -> Result<Masked, regex::Error> {
    let finder = compiled(r"\{\{[^{}]*\}\}|<ts>")?;
    let mut source = String::with_capacity(rec.len() + 32);
    source.push_str(r"\A");
    let mut numbered = Vec::new();
    let mut cursor = 0;
    for found in finder.find_iter(rec) {
        source.push_str(&regex::escape(&rec[cursor..found.start()]));
        cursor = found.end();
        let token = found.as_str();
        let named = token
            .strip_prefix("{{")
            .and_then(|rest| rest.strip_suffix("}}"))
            .map(str::trim);
        let class = named.map_or_else(
            || Some(TokenClass::Timestamp),
            |name| TokenClass::named(name).or_else(|| TokenClass::numbered(name)),
        );
        let Some(class) = class else {
            source.push_str(&regex::escape(token));
            continue;
        };
        if let Some(text) = class.concrete(masks) {
            source.push_str(&regex::escape(&text));
            continue;
        }
        match class {
            TokenClass::Pty { .. } | TokenClass::Pid { .. } => {
                let group = format!("t{}", numbered.len() + 1);
                source.push_str("(?P<");
                source.push_str(&group);
                source.push('>');
                source.push_str(class.pattern());
                source.push(')');
                numbered.push((class, group));
            }
            TokenClass::Date
            | TokenClass::Timestamp
            | TokenClass::Port
            | TokenClass::Tmp
            | TokenClass::Hostname
            | TokenClass::EpochMs
            | TokenClass::Repo
            | TokenClass::Node
            | TokenClass::NodeVersion
            | TokenClass::Version
            | TokenClass::Revision
            | TokenClass::Key
            | TokenClass::GenId => source.push_str(class.pattern()),
        }
    }
    source.push_str(&regex::escape(&rec[cursor..]));
    source.push_str(r"\z");
    Ok(Masked { source, numbered })
}

fn numbered_consistent(captures: &regex::Captures<'_>, numbered: &[(TokenClass, String)]) -> bool {
    let mut bound: Vec<(TokenClass, &str)> = Vec::new();
    for (class, group) in numbered {
        let Some(text) = captures.name(group) else {
            return false;
        };
        let text = text.as_str();
        if let Some((_, seen)) = bound.iter().find(|(bound_class, _)| bound_class == class) {
            if *seen != text {
                return false;
            }
        } else {
            if bound.iter().any(|(_, seen)| *seen == text) {
                return false;
            }
            bound.push((class.clone(), text));
        }
    }
    true
}

pub fn masked_eq(rec: &str, act: &str, masks: &Masks) -> Result<bool, regex::Error> {
    if !has_tokens(rec) {
        return Ok(rec == act);
    }
    let masked = masked_pattern(rec, masks)?;
    let regex = compiled(&masked.source)?;
    let Some(captures) = regex.captures(act) else {
        return Ok(false);
    };
    Ok(numbered_consistent(&captures, &masked.numbered))
}

#[must_use]
fn has_tokens(rec: &str) -> bool {
    rec.contains("{{") || rec.contains("<ts>")
}

fn try_value_eq<E>(
    rec: &Json,
    act: &Json,
    strings: &impl Fn(&str, &str) -> Result<bool, E>,
) -> Result<bool, E> {
    match (rec, act) {
        (Json::Object(r), Json::Object(a)) => {
            let present = |map: &serde_json::Map<String, Json>| {
                map.values().filter(|value| !value.is_null()).count()
            };
            if present(r) != present(a) {
                return Ok(false);
            }
            for (key, value) in r.iter().filter(|(_, value)| !value.is_null()) {
                let Some(other) = a.get(key).filter(|other| !other.is_null()) else {
                    return Ok(false);
                };
                if !try_value_eq(value, other, strings)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Json::Array(r), Json::Array(a)) => {
            if r.len() != a.len() {
                return Ok(false);
            }
            for (r, a) in r.iter().zip(a) {
                if !try_value_eq(r, a, strings)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Json::String(r), Json::String(a)) => strings(r, a),
        _ => Ok(rec == act),
    }
}

pub(crate) fn value_eq(rec: &Json, act: &Json, strings: &impl Fn(&str, &str) -> bool) -> bool {
    match try_value_eq::<std::convert::Infallible>(rec, act, &|r, a| Ok(strings(r, a))) {
        Ok(equal) => equal,
        Err(never) => match never {},
    }
}

#[must_use]
pub fn json_eq(a: &Json, b: &Json) -> bool {
    value_eq(a, b, &|x, y| x == y)
}

pub fn json_masked_eq(rec: &Json, act: &Json, masks: &Masks) -> Result<bool, regex::Error> {
    try_value_eq(rec, act, &|r, a| masked_eq(r, a, masks))
}

#[must_use]
pub fn substitute(text: &str, masks: &Masks) -> String {
    let mut out = text.replace("{{PORT}}", &masks.port);
    if let Some(tmp) = &masks.tmp {
        out = out.replace("{{TMP}}", tmp);
    }
    if let Some(host) = &masks.hostname {
        out = out.replace("{{HOSTNAME}}", host);
    }
    out
}
