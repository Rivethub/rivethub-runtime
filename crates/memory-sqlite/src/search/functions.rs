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

pub(super) fn literal_lower(text: &str) -> String {
    text.chars()
        .flat_map(|character| character.to_lowercase().take(1))
        .collect()
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
    conn.create_scalar_function("rh_literal_lower", 1, flags, |ctx| {
        Ok(literal_lower(&ctx.get::<String>(0)?))
    })
    .map_err(SqliteError::query("register literal case conversion"))?;
    let scorer = crate::trigrams::Scorer::new()?;
    conn.create_scalar_function("rh_similarity", 2, flags, move |ctx| {
        let left = ctx.get::<String>(0)?;
        let right = ctx.get::<String>(1)?;
        Ok(f64::from(scorer.similarity(&left, &right)))
    })
    .map_err(SqliteError::query("register similarity"))
}
