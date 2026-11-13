use std::collections::BTreeSet;
use std::collections::HashMap;

use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::compact_key;
use uuid::Uuid;

use super::error::query_error;

use super::sql::KNOWN_ALIASES;
use super::sql::KNOWN_IDS;
use super::sql::KNOWN_ROWS;

#[derive(Debug, sqlx::FromRow)]
struct KnownRow {
    id: Uuid,
    name: String,
    created_at: DateTime<Utc>,
    type_name: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct KnownAlias {
    topic_id: Uuid,
    alias: String,
}

const KNOWN_DOC_CHARS: usize = 48_000;
const KNOWN_KEY_WINDOW: usize = 4;
const KNOWN_KEY_MIN: usize = 2;
const KNOWN_KEY_MAX: usize = 200;

pub(super) async fn known_entities(
    pool: &sqlx::PgPool,
    leaves: &[EntityLeaf],
) -> Result<Vec<KnownEntity>, EntityStoreError> {
    let document = known_document(leaves);
    if document.trim().is_empty() {
        return Ok(Vec::new());
    }
    let keys = known_keys(&document);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let ids = sqlx::query_scalar::<_, Uuid>(KNOWN_IDS)
        .persistent(false)
        .bind(&keys)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, KnownRow>(KNOWN_ROWS)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let mut pending = hydrate_known(pool, rows).await?;
    order_known(&mut pending, &document);
    let limit = usize::try_from(kernel::wiki::entity_pass::KNOWN_LIMIT).map_err(|error| {
        EntityStoreError::Query(Box::new(std::io::Error::other(error.to_string())))
    })?;
    pending.truncate(limit);
    Ok(pending.into_iter().map(|row| row.entity).collect())
}

fn known_document(leaves: &[EntityLeaf]) -> String {
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

fn known_keys(document: &str) -> Vec<String> {
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

struct PendingKnown {
    score: i32,
    created_at: DateTime<Utc>,
    entity: KnownEntity,
}

async fn hydrate_known(
    pool: &sqlx::PgPool,
    rows: Vec<KnownRow>,
) -> Result<Vec<PendingKnown>, EntityStoreError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let alias_rows = sqlx::query_as::<_, KnownAlias>(KNOWN_ALIASES)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let mut facts = super::claims::known_facts(pool, &ids).await?;
    let mut aliases: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in alias_rows {
        aliases.entry(row.topic_id).or_default().push(row.alias);
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let aliases = aliases.remove(&row.id).unwrap_or_default();
            let facts = facts.remove(&row.id).unwrap_or_default();
            PendingKnown {
                score: 0,
                created_at: row.created_at,
                entity: KnownEntity {
                    id: row.id,
                    name: row.name,
                    type_name: row.type_name.unwrap_or_default(),
                    aliases,
                    facts,
                },
            }
        })
        .collect())
}

fn order_known(rows: &mut [PendingKnown], document: &str) {
    let body = document.to_lowercase();
    for row in rows.iter_mut() {
        row.score = mention_score(&body, &row.entity.name, &row.entity.aliases);
    }
    rows.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.created_at.cmp(&right.created_at))
    });
}

fn mention_score(body: &str, name: &str, aliases: &[String]) -> i32 {
    if mentioned(body, name) || aliases.iter().any(|alias| mentioned(body, alias)) {
        2
    } else {
        1
    }
}

fn mentioned(body: &str, name: &str) -> bool {
    name.chars().nth(1).is_some() && body.contains(&name.to_lowercase())
}
