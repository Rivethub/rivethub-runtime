use uuid::Uuid;

use super::entity_pass::MatchCandidate;
use super::entity_pass::compact_key;

pub const MATCH_THRESHOLD: f32 = 0.72;
pub const TRIGRAM_FLOOR: f32 = 0.30;
pub const EMBED_FLOOR: f32 = 0.45;
pub const CANDIDATE_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeRoot {
    Person,
    Org,
    Host,
    Hardware,
    Model,
    Service,
    Software,
    Repo,
    Domain,
    Database,
    Project,
    Place,
    Agent,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchAct {
    New,
    Existing(usize),
    Hold,
}

#[must_use]
pub fn type_root(type_name: &str) -> TypeRoot {
    let key = compact_key(type_name);
    if key.is_empty() {
        return TypeRoot::Other;
    }
    if key.contains("person") || key.contains("people") {
        return TypeRoot::Person;
    }
    if key.contains("organi") || key == "org" || key.contains("orgs") {
        return TypeRoot::Org;
    }
    if key.contains("host") || key.contains("machine") {
        return TypeRoot::Host;
    }
    if key.contains("hardware") || key.contains("device") {
        return TypeRoot::Hardware;
    }
    if key.contains("model") {
        return TypeRoot::Model;
    }
    if key.contains("service") {
        return TypeRoot::Service;
    }
    if key.contains("repo") {
        return TypeRoot::Repo;
    }
    if key.contains("domain") {
        return TypeRoot::Domain;
    }
    if key.contains("database") {
        return TypeRoot::Database;
    }
    if key.contains("project") {
        return TypeRoot::Project;
    }
    if key.contains("place") || key.contains("location") {
        return TypeRoot::Place;
    }
    if key.contains("agent") {
        return TypeRoot::Agent;
    }
    if key.contains("software")
        || key.contains("app")
        || key.contains("librar")
        || key.contains("plugin")
        || key.contains("cli")
        || key.contains("platform")
    {
        return TypeRoot::Software;
    }
    TypeRoot::Other
}

#[must_use]
pub fn types_compatible(left: &str, right: &str) -> bool {
    let a = type_root(left);
    let b = type_root(right);
    if a == TypeRoot::Other || b == TypeRoot::Other {
        let key = compact_key(left);
        return !key.is_empty() && key == compact_key(right);
    }
    a == b
}

#[must_use]
pub fn host_party_mismatch(left: &str, right: &str) -> bool {
    let left_root = type_root(left);
    let right_root = type_root(right);
    party_versus_host(left_root, right_root) || party_versus_host(right_root, left_root)
}

#[must_use]
pub fn merge_types_allowed(left: &str, right: &str) -> bool {
    !host_party_mismatch(left, right) && types_compatible(left, right)
}

fn party_versus_host(party: TypeRoot, host: TypeRoot) -> bool {
    matches!(party, TypeRoot::Person | TypeRoot::Org) && host == TypeRoot::Host
}

pub const TYPE_OBSERVATION_WINDOW: usize = 8;

pub const REPO_PART_MIN: usize = 6;

#[must_use]
pub fn names_same_entity(left_type: &str, right_type: &str) -> bool {
    let left = type_root(left_type) == TypeRoot::Person;
    let right = type_root(right_type) == TypeRoot::Person;
    left == right
}

#[must_use]
pub fn canonical_key(text: &str) -> String {
    compact_key(text)
}

#[must_use]
pub fn identity_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    push_key(&mut keys, &canonical_key(name));
    if let Some(tail) = repo_tail(name) {
        let key = compact_key(tail);
        if key.len() >= REPO_PART_MIN {
            push_key(&mut keys, &key);
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

#[must_use]
pub fn names_bind(left: &str, right: &str) -> bool {
    let right_keys = identity_keys(right);
    identity_keys(left)
        .into_iter()
        .any(|key| right_keys.contains(&key))
}

#[must_use]
pub fn ref_verified(observed: &str, entity_name: &str, aliases: &[String]) -> bool {
    let key = canonical_key(observed);
    if key.is_empty() {
        return false;
    }
    key == canonical_key(entity_name) || aliases.iter().any(|alias| canonical_key(alias) == key)
}

#[must_use]
pub fn name_identifies(observed: &str, entity_name: &str, aliases: &[String]) -> bool {
    if names_bind(observed, entity_name) {
        return true;
    }
    let observed_keys = identity_keys(observed);
    aliases.iter().any(|alias| {
        identity_keys(alias)
            .iter()
            .any(|key| observed_keys.contains(key))
    })
}

#[must_use]
pub fn cross_names_match(
    left_type: &str,
    left_name: &str,
    right_type: &str,
    right_name: &str,
) -> bool {
    !types_compatible(left_type, right_type)
        && names_same_entity(left_type, right_type)
        && names_bind(left_name, right_name)
}

#[must_use]
pub fn dominant_type(recent: &[String]) -> Option<&str> {
    if recent.is_empty() {
        return None;
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for name in recent {
        if let Some(slot) = counts.iter_mut().find(|(kept, _)| *kept == name.as_str()) {
            slot.1 += 1;
        } else {
            counts.push((name.as_str(), 1));
        }
    }
    let (winner, count) = counts.iter().max_by_key(|(name, seen)| (*seen, *name))?;
    (count.saturating_mul(2) > recent.len()).then_some(*winner)
}

fn push_key(keys: &mut Vec<String>, key: &str) {
    if !key.is_empty() {
        keys.push(key.to_owned());
    }
}

fn repo_tail(name: &str) -> Option<&str> {
    let (_, tail) = name.split_once('/')?;
    let tail = tail.trim();
    (!tail.is_empty()).then_some(tail)
}

#[must_use]
pub fn normalize_confidence(value: f64) -> f32 {
    let scaled = if value > 1.0 && value <= 100.0 {
        value / 100.0
    } else {
        value
    };
    if !scaled.is_finite() || scaled <= 0.0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a confidence score only needs f32 precision inside 0..=1"
    )]
    let score = scaled.clamp(0.0, 1.0) as f32;
    score
}

#[must_use]
pub fn gate(choice: &str, confidence: f32, threshold: f32, candidates: usize) -> MatchAct {
    if !confidence.is_finite() || confidence < threshold {
        return MatchAct::Hold;
    }
    let choice = choice.trim();
    if choice.eq_ignore_ascii_case("new") {
        return MatchAct::New;
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a choice that is not an index holds the match"
        )
    )]
    let Ok(index) = choice.parse::<usize>() else {
        return MatchAct::Hold;
    };
    if index < candidates {
        MatchAct::Existing(index)
    } else {
        MatchAct::Hold
    }
}

#[must_use]
pub fn parse_verdict(text: &str) -> Option<(String, f32)> {
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let body = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a verdict that is not JSON is no verdict"
        )
    )]
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let choice = match value.get("choice")? {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => return None,
    };
    let confidence = value.get("confidence")?.as_f64()?;
    Some((choice, normalize_confidence(confidence)))
}

#[must_use]
pub fn rank_candidates(candidates: Vec<MatchCandidate>) -> Vec<MatchCandidate> {
    let mut merged: Vec<MatchCandidate> = Vec::new();
    for candidate in candidates {
        if let Some(slot) = merged.iter_mut().find(|kept| kept.id == candidate.id) {
            slot.trigram = slot.trigram.max(candidate.trigram);
            slot.embedding = slot.embedding.max(candidate.embedding);
            if slot.summary.is_empty() {
                slot.summary = candidate.summary;
            }
            if slot.aliases.is_empty() {
                slot.aliases = candidate.aliases;
            }
        } else {
            merged.push(candidate);
        }
    }
    merged.retain(|candidate| {
        candidate.trigram >= TRIGRAM_FLOOR || candidate.embedding >= EMBED_FLOOR
    });
    merged.sort_by(|left, right| {
        score(right)
            .total_cmp(&score(left))
            .then(left.id.cmp(&right.id))
    });
    merged.truncate(CANDIDATE_LIMIT);
    merged
}

fn score(candidate: &MatchCandidate) -> f32 {
    candidate.trigram.max(candidate.embedding)
}

#[must_use]
pub fn exact_id(
    type_name: &str,
    name: &str,
    aliases: &[String],
    candidates: &[MatchCandidate],
) -> Option<Uuid> {
    candidates.iter().find_map(|candidate| {
        if !names_same_entity(type_name, &candidate.type_name) {
            return None;
        }
        let hit = if types_compatible(type_name, &candidate.type_name) {
            let keys = keys_of(name, aliases);
            keys_of(&candidate.name, &candidate.aliases)
                .iter()
                .any(|key| keys.contains(key))
                || names_bind(name, &candidate.name)
        } else {
            name_identifies(name, &candidate.name, &candidate.aliases)
        };
        hit.then_some(candidate.id)
    })
}

fn keys_of(name: &str, aliases: &[String]) -> Vec<String> {
    let mut keys = vec![compact_key(name)];
    keys.extend(aliases.iter().map(|alias| compact_key(alias)));
    keys.retain(|key| !key.is_empty());
    keys.sort();
    keys.dedup();
    keys
}

#[must_use]
pub fn chosen_id(type_name: &str, candidates: &[MatchCandidate], act: MatchAct) -> Option<Uuid> {
    let MatchAct::Existing(index) = act else {
        return None;
    };
    candidates.get(index).and_then(|candidate| {
        types_compatible(type_name, &candidate.type_name).then_some(candidate.id)
    })
}

#[must_use]
pub fn embed_line(name: &str, aliases: &[String], summary: &str) -> String {
    let mut lines = Vec::new();
    let name = name.trim();
    if !name.is_empty() {
        lines.push(name.to_owned());
    }
    let alias_line = aliases
        .iter()
        .map(|alias| alias.trim())
        .filter(|alias| !alias.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !alias_line.is_empty() {
        lines.push(alias_line);
    }
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let summary: String = summary.chars().take(240).collect();
    if !summary.is_empty() {
        lines.push(summary);
    }
    lines.join("\n")
}

const MARKER_MIN: usize = 3;
const MARKER_MAX: usize = 200;
pub const TYPE_ASK_LIMIT: usize = 24;

#[must_use]
pub fn same_type_name<'a>(wanted: &str, existing: &'a [String]) -> Option<&'a str> {
    let trimmed = wanted.trim();
    if trimmed.is_empty() {
        return None;
    }
    let folded = compact_key(trimmed);
    if let Some(found) = existing.iter().find(|name| {
        name.eq_ignore_ascii_case(trimmed) || (!folded.is_empty() && compact_key(name) == folded)
    }) {
        return Some(found.as_str());
    }
    let spelled: Vec<&str> = existing
        .iter()
        .filter(|name| plural_of(&folded, &compact_key(name)))
        .map(String::as_str)
        .collect();
    match spelled.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

#[must_use]
pub fn type_options<'a>(wanted: &str, existing: &'a [String]) -> Vec<&'a str> {
    if existing.len() <= TYPE_ASK_LIMIT {
        return existing.iter().map(String::as_str).collect();
    }
    let folded = compact_key(wanted);
    let mut ranked: Vec<&str> = existing.iter().map(String::as_str).collect();
    ranked.sort_by(|left, right| {
        shared_prefix(&folded, &compact_key(right))
            .cmp(&shared_prefix(&folded, &compact_key(left)))
            .then(left.cmp(right))
    });
    ranked.truncate(TYPE_ASK_LIMIT);
    ranked
}

fn shared_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .count()
}

fn plural_of(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let (short, long) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    long.strip_prefix(short)
        .is_some_and(|rest| rest == "s" || rest == "es")
}

#[must_use]
pub fn surface_markers(texts: &[String]) -> Vec<String> {
    let mut keys = Vec::new();
    for text in texts {
        for token in marker_tokens(text) {
            let Some(marker) = marker_of(token) else {
                continue;
            };
            if !keys.iter().any(|kept: &String| kept == &marker) {
                keys.push(marker);
            }
        }
    }
    keys
}

fn marker_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '"' | '\'' | '`' | '<' | '>')
    })
    .map(|token| token.trim_matches(|c: char| matches!(c, '.' | ':' | '/')))
    .filter(|token| !token.is_empty())
}

fn marker_of(token: &str) -> Option<String> {
    if token.chars().count() < MARKER_MIN || token.chars().count() > MARKER_MAX {
        return None;
    }
    if ipv4(token) {
        return Some(token.to_owned());
    }
    if let Some(host) = url_host(token)
        && host.chars().count() >= MARKER_MIN
    {
        return Some(host);
    }
    if repo_marker(token) || hostname_marker(token) {
        return Some(token.to_owned());
    }
    None
}

fn ipv4(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.parse::<u8>().is_ok()
    })
}

fn url_host(token: &str) -> Option<String> {
    let rest = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))?;
    let host = rest.split(['/', ':', '?', '#']).next()?.trim();
    (!host.is_empty()).then(|| host.to_owned())
}

fn repo_marker(token: &str) -> bool {
    let Some((_, tail)) = token.split_once('/') else {
        return false;
    };
    let tail = tail.trim();
    tail.chars().count() >= REPO_PART_MIN
        && tail
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn hostname_marker(token: &str) -> bool {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < MARKER_MIN || chars.len() > 63 {
        return false;
    }
    let letter = chars.iter().any(char::is_ascii_alphabetic);
    let marked = chars
        .iter()
        .any(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
    letter
        && marked
        && chars
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[cfg(test)]
#[path = "tests/entity_match_tests.rs"]
mod tests;
