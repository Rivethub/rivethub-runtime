use std::path::{Path, PathBuf};

use schemars::Schema;

const CONTRACT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contract");

fn rendered(schema: &Schema) -> Result<String, serde_json::Error> {
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&sorted(serde_json::to_value(schema)?))?
    ))
}

fn sorted(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sorted).collect())
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map
                .into_iter()
                .map(|(key, item)| (key, sorted(item)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(entries.into_iter().collect())
        }
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Bool(item) => serde_json::Value::Bool(item),
        serde_json::Value::Number(item) => serde_json::Value::Number(item),
        serde_json::Value::String(item) => serde_json::Value::String(item),
    }
}

fn current(name: &str, schema: &Schema) -> Result<(), String> {
    let path: PathBuf = Path::new(CONTRACT_DIR).join(name);
    let generated = rendered(schema).map_err(|error| format!("{name}: {error}"))?;
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed == generated {
        return Ok(());
    }
    std::fs::write(&path, &generated).map_err(|error| format!("{name}: {error}"))?;
    Err(format!(
        "{name} was stale and has been regenerated from wire; review and commit it"
    ))
}

#[test]
fn sent_schema_is_committed() -> Result<(), String> {
    current("sent.schema.json", &wire::contract::sent())
}

#[test]
fn accepted_schema_is_committed() -> Result<(), String> {
    current("accepted.schema.json", &wire::contract::accepted())
}

#[test]
fn every_definition_name_is_unique() -> Result<(), String> {
    for schema in [wire::contract::sent(), wire::contract::accepted()] {
        let definitions = schema
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .ok_or("the contract has no $defs")?;
        let renamed: Vec<&String> = definitions
            .keys()
            .filter(|name| {
                name.strip_suffix(|last: char| last.is_ascii_digit())
                    .is_some_and(|stem| definitions.contains_key(stem))
            })
            .collect();
        if !renamed.is_empty() {
            return Err(format!(
                "two wire types share a schema name; give one a #[schemars(rename)]: {renamed:?}"
            ));
        }
    }
    Ok(())
}
