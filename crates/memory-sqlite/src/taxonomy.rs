use jiff::Timestamp;
use std::collections::HashMap;
use std::collections::HashSet;

use kernel::memory::MemoryStoreError;
use kernel::memory::TagState;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyEntry;
use kernel::memory::TagTaxonomyMerge;
use kernel::memory::TagTaxonomyMergeResult;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::tags::invalid;
use crate::time;

struct Vocab {
    display: String,
    aliases: Vec<String>,
    reason: String,
}

impl SqliteStore {
    pub(crate) async fn taxonomy_list(
        &self,
        query: TagTaxonomyQuery,
    ) -> Result<Vec<TagTaxonomyEntry>, MemoryStoreError> {
        self.with_conn(move |conn| list_taxonomy(conn, &query))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn taxonomy_upsert(
        &self,
        upsert: TagTaxonomyUpsert,
    ) -> Result<TagTaxonomyEntry, MemoryStoreError> {
        let at = time::stamp(self.now());
        self.with_conn(move |conn| upsert_taxonomy(conn, &upsert, &at))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn taxonomy_decide(
        &self,
        decision: TagTaxonomyDecision,
    ) -> Result<u64, MemoryStoreError> {
        let at = time::stamp(self.now());
        self.with_conn(move |conn| decide_taxonomy(conn, &decision, &at))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn taxonomy_merge(
        &self,
        merge: TagTaxonomyMerge,
    ) -> Result<TagTaxonomyMergeResult, MemoryStoreError> {
        let at = time::stamp(self.now());
        self.with_conn(move |conn| merge_taxonomy(conn, &merge, &at))
            .await
            .map_err(MemoryStoreError::from)
    }
}

pub(crate) fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn list_taxonomy(
    conn: &Connection,
    query: &TagTaxonomyQuery,
) -> Result<Vec<TagTaxonomyEntry>, SqliteError> {
    let mut sql = String::from(
        "SELECT key, value, display, parent_value, aliases, state, source, reason, decided_at, \
         created_at, updated_at FROM ros_tag_taxonomy WHERE 1 = 1",
    );
    let mut params = Vec::new();
    if let Some(key) = &query.key {
        sql.push_str(" AND key = ?");
        params.push(Value::Text(key.clone()));
    }
    if let Some(state) = query.state {
        sql.push_str(" AND state = ?");
        params.push(Value::Text(state.to_string()));
    }
    sql.push_str(" ORDER BY key, value LIMIT ?");
    params.push(Value::Integer(i64::from(query.limit.get())));
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("taxonomy list"))?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), read_entry)
        .map_err(SqliteError::query("taxonomy list"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("taxonomy list"))?;
    Ok(rows)
}

fn upsert_taxonomy(
    conn: &mut Connection,
    upsert: &TagTaxonomyUpsert,
    at: &str,
) -> Result<TagTaxonomyEntry, SqliteError> {
    let key = required_slug(&upsert.key)?;
    let value = required_slug(&upsert.value)?;
    let aliases = cleaned_aliases(&upsert.aliases, &value);
    let parent = checked_parent(conn, &key, &value, upsert.parent_value.as_deref())?;
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("taxonomy upsert"))?;
    let existing = load_vocab(&tx, &key, &value)?;
    let display = chosen_display(upsert.display.as_deref(), &upsert.value, existing.as_ref());
    let reason = chosen_reason(upsert.reason.as_deref(), existing.as_ref());
    let alias_json = json_list(&aliases)?;
    match existing {
        Some(_) => {
            tx.execute(
                "UPDATE ros_tag_taxonomy SET display = ?3, parent_value = ?4, aliases = ?5, \
                 reason = ?6, updated_at = ?7 WHERE key = ?1 AND value = ?2",
                params_from_iter([
                    Value::Text(key.clone()),
                    Value::Text(value.clone()),
                    Value::Text(display),
                    parent,
                    Value::Text(alias_json),
                    Value::Text(reason),
                    Value::Text(at.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("taxonomy upsert"))?;
        }
        None => {
            tx.execute(
                "INSERT INTO ros_tag_taxonomy \
                    (key, value, display, parent_value, aliases, state, source, reason, \
                     decided_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'accepted', 'user', ?6, ?7, ?7, ?7)",
                params_from_iter([
                    Value::Text(key.clone()),
                    Value::Text(value.clone()),
                    Value::Text(display),
                    parent,
                    Value::Text(alias_json),
                    Value::Text(reason),
                    Value::Text(at.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("taxonomy upsert"))?;
        }
    }
    let entry = read_one(&tx, &key, &value)?;
    tx.commit().map_err(SqliteError::query("taxonomy upsert"))?;
    Ok(entry)
}

fn decide_taxonomy(
    conn: &Connection,
    decision: &TagTaxonomyDecision,
    at: &str,
) -> Result<u64, SqliteError> {
    let state = TagState::from(decision.decision).to_string();
    let mut changed = 0u64;
    for entry in &decision.entries {
        let updated = conn
            .execute(
                "UPDATE ros_tag_taxonomy SET state = ?3, decided_at = ?4, updated_at = ?4 \
                  WHERE key = ?1 AND value = ?2 AND state <> ?3",
                params_from_iter([
                    Value::Text(entry.key.clone()),
                    Value::Text(entry.value.clone()),
                    Value::Text(state.clone()),
                    Value::Text(at.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("taxonomy decide"))?;
        changed += u64::try_from(updated).map_err(SqliteError::Count)?;
    }
    Ok(changed)
}

fn merge_taxonomy(
    conn: &mut Connection,
    merge: &TagTaxonomyMerge,
    at: &str,
) -> Result<TagTaxonomyMergeResult, SqliteError> {
    if merge.key.is_empty() || merge.from.is_empty() || merge.from == merge.into {
        return Err(invalid("merge needs one key and two different values"));
    }
    let into_value = canonical(conn, &merge.key, &merge.into)?.ok_or_else(|| {
        invalid(format!(
            "invalid merge: {}:{} is not in the vocabulary or in use",
            merge.key, merge.into
        ))
    })?;
    let from_row = load_vocab(conn, &merge.key, &merge.from)?;
    let from_used = value_in_use(conn, &merge.key, &merge.from)?;
    let from_value = match canonical(conn, &merge.key, &merge.from)? {
        Some(value) => value,
        None if from_row.is_some() || from_used => merge.from.clone(),
        None => {
            return Err(invalid(format!(
                "invalid merge: {}:{} is not in the vocabulary or in use",
                merge.key, merge.from
            )));
        }
    };
    if from_value == into_value {
        return Ok(TagTaxonomyMergeResult {
            moved: 0,
            dropped: 0,
            into: into_value,
        });
    }
    if nested(conn, &merge.key, &into_value, &from_value)? {
        return Err(invalid(format!(
            "invalid merge: {}:{} is nested under {}:{}",
            merge.key, merge.into, merge.key, merge.from
        )));
    }
    let from_aliases = match from_row {
        Some(row) => row.aliases,
        None => Vec::new(),
    };
    apply_merge(conn, merge, &from_value, &into_value, &from_aliases, at)
}

fn apply_merge(
    conn: &mut Connection,
    merge: &TagTaxonomyMerge,
    from_value: &str,
    into_value: &str,
    from_aliases: &[String],
    at: &str,
) -> Result<TagTaxonomyMergeResult, SqliteError> {
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("taxonomy merge"))?;
    let dropped = tx
        .execute(
            "DELETE FROM ros_tags WHERE key = ?1 AND value = ?2 AND EXISTS \
               (SELECT 1 FROM ros_tags kept WHERE kept.key = ?1 AND kept.value = ?3 \
                  AND kept.entity_type = ros_tags.entity_type \
                  AND kept.entity_id = ros_tags.entity_id)",
            params_from_iter([
                Value::Text(merge.key.clone()),
                Value::Text(from_value.to_owned()),
                Value::Text(into_value.to_owned()),
            ]),
        )
        .map_err(SqliteError::query("taxonomy merge"))?;
    let moved = tx
        .execute(
            "UPDATE ros_tags SET value = ?3, updated_at = ?4 WHERE key = ?1 AND value = ?2",
            params_from_iter([
                Value::Text(merge.key.clone()),
                Value::Text(from_value.to_owned()),
                Value::Text(into_value.to_owned()),
                Value::Text(at.to_owned()),
            ]),
        )
        .map_err(SqliteError::query("taxonomy merge"))?;
    let mut aliases = aliases_of(&tx, &merge.key, into_value)?;
    push_alias(&mut aliases, from_value, into_value);
    for alias in from_aliases {
        push_alias(&mut aliases, alias, into_value);
    }
    tx.execute(
        "UPDATE ros_tag_taxonomy SET aliases = ?3, updated_at = ?4 \
          WHERE key = ?1 AND value = ?2",
        params_from_iter([
            Value::Text(merge.key.clone()),
            Value::Text(into_value.to_owned()),
            Value::Text(json_list(&aliases)?),
            Value::Text(at.to_owned()),
        ]),
    )
    .map_err(SqliteError::query("taxonomy merge"))?;
    tx.execute(
        "UPDATE ros_tag_taxonomy SET state = 'rejected', reason = ?3, aliases = '[]', \
            parent_value = NULL, decided_at = ?4, updated_at = ?4 \
          WHERE key = ?1 AND value = ?2",
        params_from_iter([
            Value::Text(merge.key.clone()),
            Value::Text(from_value.to_owned()),
            Value::Text(format!("merged into {}:{}", merge.key, into_value)),
            Value::Text(at.to_owned()),
        ]),
    )
    .map_err(SqliteError::query("taxonomy merge"))?;
    tx.commit().map_err(SqliteError::query("taxonomy merge"))?;
    Ok(TagTaxonomyMergeResult {
        moved: u64::try_from(moved).map_err(SqliteError::Count)?,
        dropped: u64::try_from(dropped).map_err(SqliteError::Count)?,
        into: into_value.to_owned(),
    })
}

fn checked_parent(
    conn: &Connection,
    key: &str,
    value: &str,
    parent: Option<&str>,
) -> Result<Value, SqliteError> {
    let Some(raw) = parent else {
        return Ok(Value::Null);
    };
    let parent_slug = slug(raw);
    if parent_slug == value {
        return Err(invalid("a value cannot be its own parent"));
    }
    if !vocab_has(conn, key, &parent_slug)? {
        return Err(invalid(format!(
            "invalid parent: {key}:{raw} is not in the vocabulary"
        )));
    }
    if nested(conn, key, &parent_slug, value)? {
        return Err(invalid("invalid parent: it would create a cycle"));
    }
    Ok(Value::Text(parent_slug))
}

fn nested(conn: &Connection, key: &str, start: &str, target: &str) -> Result<bool, SqliteError> {
    let parents = parent_map(conn, key)?;
    let mut cursor = start.to_owned();
    let mut seen = HashSet::new();
    while seen.insert(cursor.clone()) {
        if cursor == target {
            return Ok(true);
        }
        match parents.get(&cursor) {
            Some(next) => cursor.clone_from(next),
            None => return Ok(false),
        }
    }
    Ok(true)
}

fn parent_map(conn: &Connection, key: &str) -> Result<HashMap<String, String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT value, parent_value FROM ros_tag_taxonomy \
              WHERE key = ?1 AND parent_value IS NOT NULL",
        )
        .map_err(SqliteError::query("taxonomy parents"))?;
    let rows = statement
        .query_map(params_from_iter([Value::Text(key.to_owned())]), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(SqliteError::query("taxonomy parents"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("taxonomy parents"))?;
    Ok(rows.into_iter().collect())
}

fn canonical(conn: &Connection, key: &str, token: &str) -> Result<Option<String>, SqliteError> {
    if vocab_has(conn, key, token)? {
        let state = state_at(conn, key, token)?;
        if state.as_deref() == Some("accepted") {
            return Ok(Some(token.to_owned()));
        }
    }
    let mut statement = conn
        .prepare(
            "SELECT value, aliases FROM ros_tag_taxonomy WHERE key = ?1 AND state = 'accepted'",
        )
        .map_err(SqliteError::query("taxonomy alias"))?;
    let rows = statement
        .query_map(params_from_iter([Value::Text(key.to_owned())]), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(SqliteError::query("taxonomy alias"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("taxonomy alias"))?;
    for (value, aliases) in rows {
        if string_list(&aliases, "aliases")?
            .iter()
            .any(|alias| alias == token)
        {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn vocab_has(conn: &Connection, key: &str, value: &str) -> Result<bool, SqliteError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM ros_tag_taxonomy WHERE key = ?1 AND value = ?2",
            params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("taxonomy lookup"))?;
    Ok(found.is_some())
}

fn state_at(conn: &Connection, key: &str, value: &str) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        "SELECT state FROM ros_tag_taxonomy WHERE key = ?1 AND value = ?2",
        params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("taxonomy lookup"))
}

fn value_in_use(conn: &Connection, key: &str, value: &str) -> Result<bool, SqliteError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM ros_tags WHERE key = ?1 AND value = ?2",
            params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("taxonomy tags"))?;
    Ok(found.is_some())
}

fn aliases_of(conn: &Connection, key: &str, value: &str) -> Result<Vec<String>, SqliteError> {
    let text: String = conn
        .query_row(
            "SELECT aliases FROM ros_tag_taxonomy WHERE key = ?1 AND value = ?2",
            params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("taxonomy alias"))?;
    string_list(&text, "aliases")
}

fn load_vocab(conn: &Connection, key: &str, value: &str) -> Result<Option<Vocab>, SqliteError> {
    let found: Option<(String, String, String)> = conn
        .query_row(
            "SELECT display, aliases, reason FROM ros_tag_taxonomy WHERE key = ?1 AND value = ?2",
            params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(SqliteError::query("taxonomy lookup"))?;
    match found {
        None => Ok(None),
        Some((display, aliases, reason)) => Ok(Some(Vocab {
            display,
            aliases: string_list(&aliases, "aliases")?,
            reason,
        })),
    }
}

fn read_one(conn: &Connection, key: &str, value: &str) -> Result<TagTaxonomyEntry, SqliteError> {
    conn.query_row(
        "SELECT key, value, display, parent_value, aliases, state, source, reason, decided_at, \
         created_at, updated_at FROM ros_tag_taxonomy WHERE key = ?1 AND value = ?2",
        params_from_iter([Value::Text(key.to_owned()), Value::Text(value.to_owned())]),
        read_entry,
    )
    .map_err(SqliteError::query("taxonomy read"))
}

fn read_entry(row: &rusqlite::Row<'_>) -> Result<TagTaxonomyEntry, rusqlite::Error> {
    let aliases: String = row.get(4)?;
    let state: String = row.get(5)?;
    let decided_at: Option<String> = row.get(8)?;
    let created_at: String = row.get(9)?;
    let updated_at: String = row.get(10)?;
    Ok(TagTaxonomyEntry {
        key: row.get(0)?,
        value: row.get(1)?,
        display: row.get(2)?,
        parent_value: row.get(3)?,
        aliases: json_strings(&aliases)?,
        state: parse_state(&state)?,
        source: row.get(6)?,
        reason: row.get(7)?,
        decided_at: decided_at.as_deref().map(parse_time).transpose()?,
        created_at: parse_time(&created_at)?,
        updated_at: parse_time(&updated_at)?,
    })
}

fn json_strings(text: &str) -> Result<Vec<String>, rusqlite::Error> {
    serde_json::from_str(text).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(source))
    })
}

fn parse_state(text: &str) -> Result<TagState, rusqlite::Error> {
    text.parse().map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(source))
    })
}

fn parse_time(text: &str) -> Result<Timestamp, rusqlite::Error> {
    text.parse().map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(source))
    })
}

fn chosen_display(given: Option<&str>, raw_value: &str, existing: Option<&Vocab>) -> String {
    match (given, existing) {
        (Some(text), _) => text.to_owned(),
        (None, Some(row)) => row.display.clone(),
        (None, None) => sentence(raw_value),
    }
}

fn chosen_reason(given: Option<&str>, existing: Option<&Vocab>) -> String {
    match (given, existing) {
        (Some(text), _) => text.to_owned(),
        (None, Some(row)) => row.reason.clone(),
        (None, None) => String::new(),
    }
}

fn cleaned_aliases(aliases: &[String], value: &str) -> Vec<String> {
    let mut out = Vec::new();
    for alias in aliases {
        let cleaned = slug(alias);
        if cleaned.is_empty()
            || cleaned == value
            || out.iter().any(|kept: &String| kept == &cleaned)
        {
            continue;
        }
        out.push(cleaned);
    }
    out
}

fn push_alias(aliases: &mut Vec<String>, alias: &str, into_value: &str) {
    if alias.is_empty() || alias == into_value || aliases.iter().any(|kept| kept == alias) {
        return;
    }
    aliases.push(alias.to_owned());
}

fn required_slug(raw: &str) -> Result<String, SqliteError> {
    let cleaned = slug(raw);
    if cleaned.is_empty() {
        Err(invalid("key and value are required"))
    } else {
        Ok(cleaned)
    }
}

fn sentence(raw: &str) -> String {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = first.to_uppercase().collect::<String>();
    out.extend(chars.flat_map(char::to_lowercase));
    out
}

fn json_list(values: &[String]) -> Result<String, SqliteError> {
    serde_json::to_string(values).map_err(|source| SqliteError::Encode {
        column: "aliases",
        source,
    })
}

fn string_list(text: &str, column: &'static str) -> Result<Vec<String>, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::Query {
        operation: column,
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}
