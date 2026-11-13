use serde_json::Value;

use super::types::EntityIdentification;
use super::types::EntityIdentifications;
use super::types::EntityObservation;
use super::types::EntityObservations;
use super::types::EntityProse;
use super::types::EntityRewrite;

#[must_use]
pub fn observation_schema() -> schemars::Schema {
    schemars::schema_for!(EntityObservations)
}

#[must_use]
pub fn prose_schema() -> schemars::Schema {
    schemars::schema_for!(EntityProse)
}

#[must_use]
pub fn identify_schema() -> schemars::Schema {
    schemars::schema_for!(EntityIdentifications)
}

#[must_use]
pub fn rewrite_schema() -> schemars::Schema {
    schemars::schema_for!(EntityRewrite)
}

pub fn parse_observations(text: &str) -> Result<EntityObservations, serde_json::Error> {
    observations_from(parse_json_value(text)?)
}

pub fn parse_prose(text: &str) -> Result<EntityProse, serde_json::Error> {
    serde_json::from_value(parse_json_value(text)?)
}

pub fn parse_identifications(text: &str) -> Result<EntityIdentifications, serde_json::Error> {
    identifications_from(parse_json_value(text)?)
}

pub fn parse_rewrite(text: &str) -> Result<EntityRewrite, serde_json::Error> {
    serde_json::from_value(parse_json_value(text)?)
}

fn parse_json_value(text: &str) -> Result<Value, serde_json::Error> {
    let body = json_body(text);
    match serde_json::from_str::<Value>(body) {
        Ok(value) => Ok(value),
        Err(error) => {
            let Some(slice) = json_slice(body) else {
                return Err(error);
            };
            serde_json::from_str(slice)
        }
    }
}

fn json_body(text: &str) -> &str {
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    trimmed.strip_suffix("```").unwrap_or(trimmed).trim()
}

fn json_slice(text: &str) -> Option<&str> {
    let start = text.find(['{', '['])?;
    text.get(start..)
}

fn observations_from(value: Value) -> Result<EntityObservations, serde_json::Error> {
    match value {
        Value::Array(items) => {
            let observations = items
                .into_iter()
                .map(one_observation)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(EntityObservations { observations })
        }
        Value::Object(mut map) if map.contains_key("observations") => {
            let Some(items) = map.remove("observations") else {
                return serde_json::from_value(Value::Object(map));
            };
            match items {
                Value::Array(items) => {
                    let observations = items
                        .into_iter()
                        .map(one_observation)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(EntityObservations { observations })
                }
                other @ (Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Object(_)) => {
                    map.insert("observations".to_owned(), other);
                    serde_json::from_value(Value::Object(map))
                }
            }
        }
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_)) => {
            let one = one_observation(other)?;
            if one.name.trim().is_empty() {
                Ok(EntityObservations::default())
            } else {
                Ok(EntityObservations {
                    observations: vec![one],
                })
            }
        }
    }
}

fn one_observation(mut value: Value) -> Result<EntityObservation, serde_json::Error> {
    if let Some(history) = value.get_mut("history") {
        *history = normalize_history(history.take());
    }
    serde_json::from_value(value)
}

fn normalize_history(value: Value) -> Value {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    match value {
        Value::String(sentence) => Value::Array(vec![serde_json::json!({ "sentence": sentence })]),
        Value::Array(items) => Value::Array(items.into_iter().map(history_item).collect()),
        other @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_)) => other,
    }
}

fn history_item(value: Value) -> Value {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    match value {
        Value::String(sentence) => serde_json::json!({ "sentence": sentence }),
        other @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_)) => other,
    }
}

fn identifications_from(value: Value) -> Result<EntityIdentifications, serde_json::Error> {
    match value {
        Value::Array(items) => Ok(EntityIdentifications {
            observations: items
                .into_iter()
                .map(one_identification)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        Value::Object(mut map) => {
            if let Some(items) = map.remove("observations") {
                return identifications_from(items);
            }
            let one: EntityIdentification = serde_json::from_value(Value::Object(map))?;
            if one.name.trim().is_empty() {
                Ok(EntityIdentifications::default())
            } else {
                Ok(EntityIdentifications {
                    observations: vec![one],
                })
            }
        }
        Value::Null => Ok(EntityIdentifications::default()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => Err(serde::de::Error::custom(
            "identification JSON must be an object or array",
        )),
    }
}

fn one_identification(value: Value) -> Result<EntityIdentification, serde_json::Error> {
    serde_json::from_value(value)
}
