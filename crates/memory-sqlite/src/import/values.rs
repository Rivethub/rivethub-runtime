use rusqlite::types::Value;

use super::{ImportError, copy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Conversion {
    Text,
    Integer,
    Real,
    Boolean,
    Timestamp,
    Object,
    Payload,
    Array,
    Vector,
    Bytea,
}

impl Conversion {
    pub(super) fn for_column(name: &str, sql_type: &str, line: usize) -> Result<Self, ImportError> {
        match name {
            "active" | "range_verified" | "historical_backlog" | "index_needs_repair" => {
                Ok(Self::Boolean)
            }
            "embedding" => Ok(Self::Vector),
            "settings" | "metadata" | "tool_args" => Ok(Self::Object),
            "payload" => Ok(Self::Payload),
            "aliases" | "tags" | "entities" | "related" | "topics_touched" => Ok(Self::Array),
            _ if name.ends_with("_at") => Ok(Self::Timestamp),
            _ => match sql_type {
                "BLOB" => Ok(Self::Bytea),
                "TEXT" => Ok(Self::Text),
                "INTEGER" => Ok(Self::Integer),
                "REAL" => Ok(Self::Real),
                _ => Err(copy::invalid(line, "unsupported target column type")),
            },
        }
    }

    pub(super) fn convert(
        self,
        text: String,
        line: usize,
        width: &mut Option<usize>,
    ) -> Result<Value, ImportError> {
        match self {
            Self::Text => Ok(Value::Text(text)),
            Self::Integer => text
                .parse::<i64>()
                .map(Value::Integer)
                .map_err(|_| copy::invalid(line, "invalid integer")),
            Self::Real => {
                let number: f64 = text
                    .parse()
                    .map_err(|_| copy::invalid(line, "invalid real number"))?;
                if !number.is_finite() {
                    return Err(copy::invalid(line, "non-finite real number"));
                }
                Ok(Value::Real(number))
            }
            Self::Boolean => match text.as_bytes() {
                b"t" => Ok(Value::Integer(1)),
                b"f" => Ok(Value::Integer(0)),
                _ => Err(copy::invalid(line, "invalid Postgres boolean")),
            },
            Self::Timestamp => Ok(Value::Text(crate::time::stamp(crate::time::stored(
                &text,
                "pg_dump timestamp",
            )?))),
            Self::Object => {
                let object: wire::JsonObject = serde_json::from_str(&text)?;
                Ok(Value::Text(serde_json::to_string(&object)?))
            }
            Self::Payload => {
                let payload: crate::JobPayload = serde_json::from_str(&text)?;
                Ok(Value::Text(serde_json::to_string(&payload)?))
            }
            Self::Array => Ok(Value::Text(serde_json::to_string(&array(&text, line)?)?)),
            Self::Bytea => text
                .strip_prefix("\\x")
                .and_then(|hex| hex::decode(hex).ok())
                .map(Value::Blob)
                .ok_or_else(|| copy::invalid(line, "invalid hexadecimal bytea")),
            Self::Vector => {
                let mut vector: Vec<f32> = serde_json::from_str(&text)?;
                if width.is_some_and(|expected| expected != vector.len())
                    || !crate::vector::normalize(&mut vector)
                {
                    return Err(ImportError::Vector);
                }
                *width = Some(vector.len());
                Ok(Value::Blob(crate::vector::pack(&vector)))
            }
        }
    }
}

fn array(text: &str, line: usize) -> Result<Vec<String>, ImportError> {
    let body = text
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
        .ok_or_else(|| copy::invalid(line, "expected a one-dimensional Postgres text array"))?;
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = body.chars().peekable();
    let mut result = Vec::new();
    loop {
        let quoted = input.peek() == Some(&'"');
        if quoted {
            input.next();
        }
        let mut element = String::new();
        let mut closed = !quoted;
        while let Some(character) = input.next() {
            match character {
                '\\' => element.push(
                    input
                        .next()
                        .ok_or_else(|| copy::invalid(line, "unfinished array escape"))?,
                ),
                '"' if quoted => {
                    closed = true;
                    break;
                }
                ',' if !quoted => return Err(copy::invalid(line, "unexpected array delimiter")),
                '{' | '}' | '"' if !quoted => {
                    return Err(copy::invalid(line, "invalid array element"));
                }
                _ => element.push(character),
            }
            if !quoted && (input.peek() == Some(&',') || input.peek().is_none()) {
                break;
            }
        }
        if !closed || (!quoted && (element.is_empty() || element == "NULL")) {
            return Err(copy::invalid(
                line,
                "NULL, empty unquoted or unterminated array element",
            ));
        }
        result.push(element);
        match input.next() {
            None => break,
            Some(',') if input.peek().is_some() => {}
            Some(_) => return Err(copy::invalid(line, "invalid array separator")),
        }
    }
    Ok(result)
}
