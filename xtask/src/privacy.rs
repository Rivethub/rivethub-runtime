use std::collections::HashSet;
use std::hash::BuildHasher;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::Result;

#[derive(Debug, Default, Deserialize)]
pub struct DenylistFile {
    #[serde(default)]
    pub sha256: Vec<String>,
}

#[derive(Debug)]
pub struct PrivacyRes {
    pub uuid: Regex,
    pub uuid_full: Regex,
    pub email: Regex,
    pub home: Regex,
    pub ipv4: Regex,
    pub ipv4_full: Regex,
    pub ipv6_internal: Regex,
    pub wg_key: Regex,
    pub lan_host: Regex,
    pub word: Regex,
    pub nonspace: Regex,
    pub hyphen: Regex,
    pub title: Regex,
    pub linux_after: Regex,
    pub agent_word: Regex,
    pub host_like: Regex,
    pub tokens: Vec<(Regex, &'static str)>,
}

#[expect(clippy::expect_used)]
pub static RES: LazyLock<PrivacyRes> = LazyLock::new(|| {
    PrivacyRes::new().expect("the privacy denylist patterns are constants that compile")
});

pub fn res() -> &'static PrivacyRes {
    &RES
}

impl PrivacyRes {
    fn new() -> std::result::Result<Self, regex::Error> {
        Ok(Self {
            uuid: Regex::new(
                r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
            )?,
            uuid_full: Regex::new(
                r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
            )?,
            email: Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")?,
            home: Regex::new(r"/home/([A-Za-z0-9._-]+)")?,
            ipv4: Regex::new(r"\b(\d{1,3}(?:\.\d{1,3}){1,3})\b")?,
            ipv4_full: Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,2})?\b")?,
            ipv6_internal: Regex::new(r"(?i)\b(?:f[cd][0-9a-f]{2}|fe80)(?::[0-9a-f]{0,4}){2,}\b")?,
            wg_key: Regex::new(r"\b[A-Za-z0-9+/]{43}=")?,
            lan_host: Regex::new(r"(?i)\b[a-z0-9][a-z0-9-]*\.lan\b")?,
            word: Regex::new(r"\b[A-Za-z][A-Za-z0-9]*\b")?,
            nonspace: Regex::new(r"\S+")?,
            hyphen: Regex::new(r"\b[A-Za-z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)+\b")?,
            title: Regex::new(r"^[A-Z][a-z]+$")?,
            linux_after: Regex::new(r"^\s+Linux\b")?,
            agent_word: Regex::new(r"(?i)(^|-)(rivet|grokbot)(-|$)")?,
            host_like: Regex::new(r"^[A-Za-z]{2,}\d+$")?,
            tokens: vec![
                (
                    Regex::new(r"\bsk-[A-Za-z0-9_-]{20,}")?,
                    "API key shape (sk-…)",
                ),
                (Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{30,}")?, "GitHub token"),
                (
                    Regex::new(r"\bxox[abprs]-[A-Za-z0-9-]{10,}")?,
                    "Slack token",
                ),
                (Regex::new(r"\bAKIA[0-9A-Z]{16}\b")?, "AWS access key"),
                (
                    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")?,
                    "private key block",
                ),
                (
                    Regex::new(r"\b[0-9]{17,19}:[A-Za-z0-9_-]{35}\b")?,
                    "Telegram bot token",
                ),
                (
                    Regex::new(r"\bMT[A-Za-z0-9]{22,}\.[A-Za-z0-9_-]{6}\.[A-Za-z0-9_-]{27,}")?,
                    "Discord bot token",
                ),
            ],
        })
    }
}

pub fn sha256(value: &str) -> String {
    const DIGITS: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(value.as_bytes()) {
        #[expect(clippy::indexing_slicing)]
        let pair = [
            DIGITS[usize::from(byte >> 4)],
            DIGITS[usize::from(byte & 0x0f)],
        ];
        hex.extend(pair);
    }
    hex
}

pub fn load_hashes(root: &Path, name: &str, findings: &mut Vec<String>) -> Result<HashSet<String>> {
    let path = root.join("xtask").join("data").join(name);
    if !path.exists() {
        findings.push(format!("DENYLIST missing: xtask/data/{name}"));
        return Ok(HashSet::new());
    }
    let data: DenylistFile = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    Ok(data
        .sha256
        .into_iter()
        .filter(|hash| hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|hash| hash.to_lowercase())
        .collect())
}

pub fn allowed_home_users() -> &'static [&'static str] {
    &[
        "ubuntu", "runner", "rivet", "rivetos", "rivethub", "user", "example",
    ]
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

fn is_wg_tail(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'='
}

fn is_private_ip(token: &str) -> Option<&'static str> {
    let head = token.split('/').next()?;
    let mut parts = Vec::new();
    for piece in head.split('.') {
        parts.push(piece.parse::<u32>().ok()?);
    }
    if parts.iter().any(|&part| part > 255) {
        return None;
    }
    let (Some(a), Some(b)) = (parts.first().copied(), parts.get(1).copied()) else {
        return None;
    };
    if a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 100 && (64..=127).contains(&b))
    {
        return Some("private");
    }
    if a == 0
        || a == 127
        || (a == 169 && b == 254)
        || a >= 224
        || (a == 192 && b == 0)
        || (a == 198 && matches!(b, 18 | 19 | 51))
        || (a == 203 && b == 0)
    {
        return None;
    }
    Some("public")
}

fn in_tag_context(line: &str, idx: usize, token: &str) -> bool {
    let r = res();
    let bytes = line.as_bytes();
    let end = idx + token.len();
    let mut i = idx;
    while i > 0 && bytes.get(i - 1).is_some_and(|&b| is_ident_byte(b)) {
        i -= 1;
    }
    if bytes.get(i..i + 2) == Some(b"--") {
        return false;
    }
    let before_char = if idx >= 2 && bytes.get(idx - 1) == Some(&b'-') {
        line[..idx - 1].chars().next_back()
    } else {
        None
    };
    let hyphen_before = before_char.is_some_and(char::is_alphanumeric);
    let hyphen_after = end + 1 < line.len()
        && bytes.get(end) == Some(&b'-')
        && line[end + 1..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric);
    if hyphen_before || hyphen_after {
        let mut j = end;
        while j < line.len() && bytes.get(j).is_some_and(|&b| is_ident_byte(b)) {
            j += 1;
        }
        return line
            .get(i..j)
            .is_some_and(|word| r.agent_word.is_match(word));
    }
    let quote = if idx > 0 {
        bytes.get(idx - 1).copied()
    } else {
        None
    };
    let quoted = matches!(quote, Some(b'\'' | b'"' | b'`')) && bytes.get(end) == quote.as_ref();
    if quoted {
        if token.eq_ignore_ascii_case("arch") {
            return false;
        }
        let c = if idx >= 2 {
            line[..idx - 1].chars().next_back()
        } else {
            None
        };
        if c.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '[' | '(' | ',')) {
            return false;
        }
        return !trailing_comma_before(bytes, idx);
    }
    false
}

fn trailing_comma_before(bytes: &[u8], idx: usize) -> bool {
    let mut k = idx.checked_sub(2);
    while let Some(at) = k {
        if matches!(bytes.get(at), Some(b' ' | b'\t')) {
            k = at.checked_sub(1);
        } else {
            break;
        }
    }
    matches!(k, Some(at) if bytes.get(at) == Some(&b','))
}

fn check_persona(line: &str, idx: usize, token: &str) -> bool {
    if token.len() > 4 {
        return true;
    }
    let r = res();
    if r.title.is_match(token) {
        return !line
            .get(idx + token.len()..)
            .is_some_and(|rest| r.linux_after.is_match(rest));
    }
    in_tag_context(line, idx, token)
}

type Hits = Vec<(String, String)>;

fn denylisted_names<S: BuildHasher>(
    r: &PrivacyRes,
    line: &str,
    owner_file: bool,
    priv_deny: &HashSet<String, S>,
    out: &mut Hits,
) {
    for found in r.uuid.find_iter(line) {
        let token = found.as_str();
        if priv_deny.contains(&sha256(&token.to_lowercase())) {
            out.push(("denylist-uuid".into(), token.into()));
        }
    }
    for found in r.email.find_iter(line) {
        let token = found.as_str();
        if !owner_file && priv_deny.contains(&sha256(&token.to_lowercase())) {
            out.push(("denylist-email".into(), token.into()));
        }
    }
    let bytes = line.as_bytes();
    for found in r.hyphen.find_iter(line) {
        let token = found.as_str();
        if r.uuid_full.is_match(token) {
            continue;
        }
        let mut i = found.start();
        while i > 0 && bytes.get(i - 1).is_some_and(|&b| is_ident_byte(b)) {
            i -= 1;
        }
        if line.get(i..i + 2) == Some("--") {
            continue;
        }
        let lower = token.to_lowercase();
        let parts: Vec<&str> = lower.split('-').collect();
        let agent_tag = parts.contains(&"rivet") || parts.contains(&"grokbot");
        let tag_hit = priv_deny.contains(&sha256(&lower))
            || (agent_tag
                && parts
                    .iter()
                    .any(|part| !part.is_empty() && priv_deny.contains(&sha256(part))));
        if tag_hit {
            out.push(("denylist-tag".into(), token.into()));
        }
    }
    for found in r.word.find_iter(line) {
        let token = found.as_str();
        if check_persona(line, found.start(), token) {
            if !owner_file && priv_deny.contains(&sha256(&token.to_lowercase())) {
                out.push(("denylist-persona".into(), token.into()));
            }
        } else if r.host_like.is_match(token) && priv_deny.contains(&sha256(&token.to_lowercase()))
        {
            out.push(("denylist-host".into(), token.into()));
        }
    }
}

fn denylisted_addresses<S: BuildHasher>(
    r: &PrivacyRes,
    line: &str,
    pragma: bool,
    priv_deny: &HashSet<String, S>,
    out: &mut Hits,
) {
    for found in r.ipv4.captures_iter(line) {
        let Some(token) = found.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let parts: Vec<&str> = token.split('.').collect();
        let mut cands = vec![token.to_string()];
        for n in [3usize, 2] {
            if let Some(prefix) = parts.get(..n) {
                cands.push(prefix.join("."));
            }
        }
        if cands.iter().any(|cand| priv_deny.contains(&sha256(cand))) {
            out.push(("denylist-ip-prefix".into(), token.into()));
        }
    }
    for found in r.ipv4_full.find_iter(line) {
        let token = found.as_str();
        if !pragma && is_private_ip(token) == Some("private") {
            out.push(("private-ip".into(), token.into()));
        }
    }
}

fn home_paths<S: BuildHasher>(
    r: &PrivacyRes,
    line: &str,
    marker: bool,
    priv_deny: &HashSet<String, S>,
    out: &mut Hits,
) {
    for caps in r.home.captures_iter(line) {
        let Some(user) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if allowed_home_users().contains(&user) {
            continue;
        }
        if priv_deny.contains(&sha256(&user.to_lowercase())) || marker {
            let matched = caps.get(0).map_or("", |m| m.as_str());
            out.push(("home-path".into(), matched.into()));
        }
    }
}

fn unmasked_secrets(r: &PrivacyRes, line: &str, fixture: bool, out: &mut Hits) {
    let bytes = line.as_bytes();
    for found in r.ipv6_internal.find_iter(line) {
        out.push(("ipv6-internal".into(), found.as_str().into()));
    }
    for found in r.wg_key.find_iter(line) {
        if bytes.get(found.end()).is_none_or(|b| !is_wg_tail(*b)) {
            let shown: String = found.as_str().chars().take(12).collect();
            out.push(("wg-key".into(), format!("{shown}…")));
        }
    }
    for found in r.lan_host.find_iter(line) {
        if bytes.get(found.end()).is_none_or(|&b| b != b'.') {
            out.push(("lan-host".into(), found.as_str().into()));
        }
    }
    if !fixture {
        for (rx, label) in &r.tokens {
            if rx.is_match(line) {
                out.push(("secret".into(), (*label).into()));
            }
        }
    }
}

pub fn scan_line<S: BuildHasher>(
    line: &str,
    file: &str,
    priv_deny: &HashSet<String, S>,
    sec_deny: &HashSet<String, S>,
) -> Hits {
    let r = res();
    let mut out = Hits::new();
    let marker = is_marker_file(file);
    let owner_file = is_owner_identity_file(file);
    let fixture = is_fixture_file(file);
    let pragma = line.contains("secret-scan-allow") || line.contains("gate-allow");
    denylisted_names(r, line, owner_file, priv_deny, &mut out);
    denylisted_addresses(r, line, pragma, priv_deny, &mut out);
    home_paths(r, line, marker, priv_deny, &mut out);
    if !pragma {
        unmasked_secrets(r, line, fixture, &mut out);
    }
    for found in r.nonspace.find_iter(line) {
        let token = found.as_str();
        if token.chars().count() >= 8
            && (sec_deny.contains(&sha256(token))
                || sec_deny.contains(&sha256(&token.to_lowercase())))
        {
            let shown: String = token.chars().take(6).collect();
            out.push(("denylist-secret".into(), format!("{shown}…")));
        }
    }
    out
}

pub fn scan_text<S: BuildHasher>(
    findings: &mut Vec<String>,
    text: &str,
    file: &str,
    priv_deny: &HashSet<String, S>,
    sec_deny: &HashSet<String, S>,
    offset_lines: Option<&[usize]>,
) {
    for (index, line) in text.split('\n').enumerate() {
        for (rule, matched) in scan_line(line, file, priv_deny, sec_deny) {
            let where_line = offset_lines
                .and_then(|offsets| offsets.get(index))
                .copied()
                .unwrap_or(index + 1);
            findings.push(format!("PRIVACY {file}:{where_line}: {rule}: {matched}"));
        }
    }
}

fn at_root_or_slash(file: &str, dir: &str) -> bool {
    file == dir || file.starts_with(&format!("{dir}/")) || file.contains(&format!("/{dir}/"))
}

pub fn is_marker_file(file: &str) -> bool {
    let lower = file.to_lowercase();
    at_root_or_slash(file, "docs")
        || at_root_or_slash(file, "fixtures")
        || lower.strip_suffix(".md").is_some()
        || lower.strip_suffix(".mdx").is_some()
        || is_readme(file)
}

fn is_readme(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or("");
    match name.split_once('.') {
        Some((stem, ext)) => {
            stem.eq_ignore_ascii_case("readme")
                && !ext.is_empty()
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => name.eq_ignore_ascii_case("readme"),
    }
}

pub fn is_owner_identity_file(file: &str) -> bool {
    file == "LICENSE"
        || file.ends_with("/LICENSE")
        || file == "NOTICE"
        || file.ends_with("/NOTICE")
        || file == ".github/CODEOWNERS"
}

pub fn is_fixture_file(file: &str) -> bool {
    ["fixtures", "testdata", "golden"]
        .iter()
        .any(|dir| at_root_or_slash(file, dir))
}

pub fn skip_scan(file: &str) -> bool {
    file == "xtask/data/privacy-denylist.json"
        || file == "xtask/data/secret-denylist.json"
        || file.ends_with("/Cargo.lock")
        || file == "Cargo.lock"
        || has_binary_extension(file)
}

fn has_binary_extension(file: &str) -> bool {
    let lower = file.to_lowercase();
    binary_extensions()
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

fn binary_extensions() -> &'static [&'static str] {
    &[
        "png", "jpg", "jpeg", "gif", "webp", "ico", "svg", "pdf", "zip", "gz", "woff", "woff2",
        "ttf", "otf", "mp3", "wav", "ogg", "mp4", "webm", "so", "a", "bin", "wasm",
    ]
}
