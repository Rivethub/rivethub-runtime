use std::collections::BTreeSet;

use super::{EntityLeaf, compact_key};

const KNOWN_DOC_CHARS: usize = 48_000;
const KNOWN_KEY_WINDOW: usize = 4;
const KNOWN_KEY_MIN: usize = 2;
const KNOWN_KEY_MAX: usize = 200;

pub fn known_document(leaves: &[EntityLeaf]) -> String {
    let mut document = String::new();
    for leaf in leaves {
        if document.len() > KNOWN_DOC_CHARS {
            break;
        }
        document.push_str(&leaf.content);
        document.push('\n');
    }
    document
}

struct KnownToken {
    compact: String,
    extras: Vec<String>,
}

pub fn known_keys(document: &str) -> Vec<String> {
    let tokens = known_tokens(document);
    let mut keys = BTreeSet::new();
    for width in 1..=KNOWN_KEY_WINDOW {
        for window in tokens.windows(width) {
            let mut compact = String::new();
            for token in window {
                compact.push_str(&token.compact);
            }
            keep_key(&mut keys, &compact);
        }
    }
    for token in &tokens {
        for extra in &token.extras {
            keep_key(&mut keys, extra);
        }
    }
    keys.into_iter().collect()
}

fn keep_key(keys: &mut BTreeSet<String>, key: &str) {
    if (KNOWN_KEY_MIN..=KNOWN_KEY_MAX).contains(&key.len()) {
        keys.insert(key.to_owned());
    }
}

fn known_tokens(document: &str) -> Vec<KnownToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in document.chars() {
        if known_token_char(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            push_known_token(&mut tokens, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_known_token(&mut tokens, &current);
    }
    tokens
}

fn known_token_char(ch: char) -> bool {
    ch.is_alphanumeric() || known_gap(ch)
}

fn known_gap(ch: char) -> bool {
    matches!(ch, '/' | '.' | '-' | '_')
}

fn push_known_token(tokens: &mut Vec<KnownToken>, raw: &str) {
    let compact = compact_key(raw);
    if compact.is_empty() {
        return;
    }
    tokens.push(KnownToken {
        compact,
        extras: token_extras(raw),
    });
}

fn token_extras(raw: &str) -> Vec<String> {
    let mut extras = Vec::new();
    if let Some((_, tail)) = raw.split_once('/') {
        push_extra(&mut extras, &compact_key(tail));
        push_extra(&mut extras, &compact_key(leading_token(tail)));
    }
    for part in raw.split(known_gap) {
        push_extra(&mut extras, &compact_key(part));
    }
    extras
}

fn push_extra(extras: &mut Vec<String>, key: &str) {
    if !key.is_empty() {
        extras.push(key.to_owned());
    }
}

fn leading_token(text: &str) -> &str {
    let Some(start) = text.find(|ch: char| ch.is_alphanumeric()) else {
        return "";
    };
    let Some(rest) = text.get(start..) else {
        return "";
    };
    let end = rest
        .char_indices()
        .find(|(_, ch)| !ch.is_alphanumeric())
        .map_or(rest.len(), |(index, _)| index);
    let Some(token) = rest.get(..end) else {
        return "";
    };
    token
}

pub fn mention_score(body: &str, name: &str, aliases: &[String]) -> i32 {
    if mentioned(body, name) || aliases.iter().any(|alias| mentioned(body, alias)) {
        2
    } else {
        1
    }
}

fn mentioned(body: &str, name: &str) -> bool {
    name.chars().nth(1).is_some() && body.contains(&name.to_lowercase())
}
