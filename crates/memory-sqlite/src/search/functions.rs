use std::collections::BTreeSet;

use regex::{Regex, RegexBuilder};
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

use crate::SqliteError;

const REGEX_SIZE_LIMIT: usize = 1 << 20;

pub(crate) fn compile(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
}

pub(crate) fn register(conn: &Connection) -> Result<(), SqliteError> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("regexp", 2, flags, |ctx| {
        let regex = ctx.get_or_create_aux(0, |value| -> Result<Regex, kernel::BoxError> {
            Ok(compile(value.as_str()?)?)
        })?;
        let text = ctx.get::<Option<String>>(1)?;
        Ok(text.map(|text| regex.is_match(&text)))
    })
    .map_err(SqliteError::query("register regexp"))?;
    conn.create_scalar_function("rh_similarity", 2, flags, |ctx| {
        let left = ctx.get::<String>(0)?;
        let right = ctx.get::<String>(1)?;
        similarity(&left, &right)
    })
    .map_err(SqliteError::query("register similarity"))
}

fn trigrams(text: &str) -> BTreeSet<[char; 3]> {
    let mut result = BTreeSet::new();
    let lowered = text.to_lowercase();
    for word in lowered
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        let padded: Vec<char> = [' ', ' ']
            .into_iter()
            .chain(word.chars())
            .chain([' '])
            .collect();
        for triple in padded.array_windows::<3>() {
            result.insert(*triple);
        }
    }
    result
}

fn similarity(left: &str, right: &str) -> rusqlite::Result<f64> {
    let left = trigrams(left);
    let right = trigrams(right);
    let shared = left.intersection(&right).count();
    let union = left.len() + right.len() - shared;
    if union == 0 {
        return Ok(0.0);
    }
    let count = |value| {
        u32::try_from(value).map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
    };
    #[expect(clippy::cast_possible_truncation)]
    let ratio = (f64::from(count(shared)?) / f64::from(count(union)?)) as f32;
    Ok(f64::from(ratio))
}
