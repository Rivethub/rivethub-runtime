use uuid::Uuid;

use super::super::entity_match::canonical_key;
use super::super::entity_match::merge_types_allowed;
use super::super::entity_pass::KnownEntity;
use super::super::entity_pass::compact_key;
use super::classify::canonical_ip;
use super::classify::first_ipv4;
use super::classify::is_address;

#[must_use]
pub fn evidenced_aliases(name: &str, aliases: &[String], evidence: &str) -> Vec<String> {
    let own = compact_key(name);
    aliases
        .iter()
        .filter(|alias| {
            let key = compact_key(alias);
            !key.is_empty() && key != own && alias_in_evidence(alias, evidence)
        })
        .cloned()
        .collect()
}

#[must_use]
pub fn alias_in_evidence(alias: &str, evidence: &str) -> bool {
    let key = compact_key(alias);
    key.len() >= 2 && compact_key(evidence).contains(&key)
}

#[must_use]
pub fn alias_is_address(alias: &str) -> bool {
    is_address(alias.trim())
}

#[must_use]
pub fn address_tied_to_name(alias: &str, entity_name: &str, mention: &str) -> bool {
    if !alias_is_address(alias) {
        return false;
    }
    let ip = canonical_ip(alias.trim());
    let name = canonical_key(entity_name);
    if ip.is_empty() || name.len() < 2 {
        return false;
    }
    mention_contains_ip(mention, &ip) && compact_key(mention).contains(&name)
}

fn mention_contains_ip(mention: &str, ip: &str) -> bool {
    let mut rest = mention;
    for _ in 0..64 {
        let Some(start) = rest.find(|ch: char| ch.is_ascii_digit()) else {
            return false;
        };
        let Some(tail) = rest.get(start..) else {
            return false;
        };
        if first_ipv4(tail).is_some_and(|found| found == ip) {
            return true;
        }
        let Some(next) = tail.get(1..) else {
            return false;
        };
        rest = next;
    }
    false
}

#[must_use]
pub fn alias_claims_known(alias: &str, self_name: &str, known: &[KnownEntity]) -> bool {
    let key = canonical_key(alias);
    let own = canonical_key(self_name);
    if key.is_empty() || key == own {
        return false;
    }
    known
        .iter()
        .any(|entity| canonical_key(&entity.name) == key)
}

#[must_use]
pub fn merge_partner<'a>(
    alias: &str,
    self_id: Uuid,
    self_type: &str,
    known: &'a [KnownEntity],
) -> Option<&'a KnownEntity> {
    let key = canonical_key(alias);
    if key.is_empty() {
        return None;
    }
    known.iter().find(|entity| {
        entity.id != self_id
            && merge_types_allowed(self_type, &entity.type_name)
            && entity_key_matches(entity, &key)
    })
}

fn entity_key_matches(entity: &KnownEntity, key: &str) -> bool {
    canonical_key(&entity.name) == key
        || entity
            .aliases
            .iter()
            .any(|alias| canonical_key(alias) == key)
}

#[must_use]
pub fn names_share_mention(left: &str, right: &str, mention: &str) -> bool {
    let left = left.trim().to_lowercase();
    let right = right.trim().to_lowercase();
    if left.is_empty() || right.is_empty() || canonical_key(&left) == canonical_key(&right) {
        return false;
    }
    let haystack = mention.to_lowercase();
    token_present(&haystack, &left) && token_present(&haystack, &right)
}

fn token_present(haystack: &str, needle: &str) -> bool {
    find_token(haystack, needle).is_some()
}

fn find_token(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut from = 0;
    while from < haystack.len() {
        let tail = haystack.get(from..)?;
        let rel = tail.find(needle)?;
        let start = from.saturating_add(rel);
        let end = start.saturating_add(needle.len());
        if token_edges(haystack, start, end) {
            return Some(start);
        }
        let step = haystack
            .get(start..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8);
        let next = start.saturating_add(step);
        if next <= start {
            return None;
        }
        from = next;
    }
    None
}

fn token_edges(haystack: &str, start: usize, end: usize) -> bool {
    let before = haystack
        .get(..start)
        .and_then(|prefix| prefix.chars().next_back());
    let after = haystack.get(end..).and_then(|suffix| suffix.chars().next());
    before.is_none_or(|ch| !ch.is_alphanumeric()) && after.is_none_or(|ch| !ch.is_alphanumeric())
}
