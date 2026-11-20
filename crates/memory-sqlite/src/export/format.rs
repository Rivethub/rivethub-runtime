use rusqlite::types::Value;

use crate::SqliteError;

enum Kind {
    Bool,
    Vector,
    Bytea,
    Array,
    Real,
    Integer,
    Object,
    Text,
}

pub(super) fn field(name: &str, value: Value) -> Result<String, SqliteError> {
    if matches!(value, Value::Null) {
        return Ok(String::from("\\N"));
    }
    let text = match kind(name) {
        Kind::Bool => boolean(&value)?,
        Kind::Vector => vector(value)?,
        Kind::Bytea => match value {
            Value::Blob(bytes) => format!("\\x{}", hex::encode(bytes)),
            Value::Null | Value::Integer(_) | Value::Real(_) | Value::Text(_) => {
                return Err(invalid(name));
            }
        },
        Kind::Array => array(value)?,
        Kind::Real => real(&value)?,
        Kind::Integer => integer(&value)?,
        Kind::Object => object(name, value)?,
        Kind::Text => text(value)?,
    };
    Ok(escape(&text))
}

fn kind(name: &str) -> Kind {
    match name {
        "active" | "range_verified" | "historical_backlog" | "index_needs_repair" => Kind::Bool,
        "embedding" => Kind::Vector,
        "input_key" | "index_key" | "render_key" => Kind::Bytea,
        "aliases" | "tags" | "entities" | "related" | "topics_touched" => Kind::Array,
        "confidence" => Kind::Real,
        "access_count"
        | "embed_failures"
        | "depth"
        | "message_count"
        | "ordinal"
        | "pipeline_version"
        | "history_count"
        | "summary_ordinal"
        | "issued_part_ordinal"
        | "input_token_estimate"
        | "source_first"
        | "source_last"
        | "quality_flags"
        | "section_char_start"
        | "section_char_end"
        | "completed_version" => Kind::Integer,
        "settings" | "metadata" | "tool_args" => Kind::Object,
        _ => Kind::Text,
    }
}

fn boolean(value: &Value) -> Result<String, SqliteError> {
    match value {
        Value::Integer(1) => Ok(String::from("t")),
        Value::Integer(0) => Ok(String::from("f")),
        Value::Null | Value::Integer(_) | Value::Real(_) | Value::Text(_) | Value::Blob(_) => {
            Err(invalid("active"))
        }
    }
}

fn vector(value: Value) -> Result<String, SqliteError> {
    match value {
        Value::Blob(bytes) => {
            let values =
                crate::vector::unpack(&bytes).map_err(|()| SqliteError::StoredEmbedding)?;
            serde_json::to_string(&values).map_err(|source| SqliteError::Encode {
                column: "embedding",
                source,
            })
        }
        Value::Null | Value::Integer(_) | Value::Real(_) | Value::Text(_) => {
            Err(SqliteError::StoredEmbedding)
        }
    }
}

fn array(value: Value) -> Result<String, SqliteError> {
    let Value::Text(text) = value else {
        return Err(invalid("array"));
    };
    let items: Vec<String> = serde_json::from_str(&text).map_err(|source| SqliteError::Encode {
        column: "array",
        source,
    })?;
    Ok(postgres_array(&items))
}

fn postgres_array(items: &[String]) -> String {
    let mut out = String::from("{");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        for character in item.chars() {
            if character == '\\' || character == '"' {
                out.push('\\');
            }
            out.push(character);
        }
        out.push('"');
    }
    out.push('}');
    out
}

fn real(value: &Value) -> Result<String, SqliteError> {
    match value {
        Value::Real(number) if number.is_finite() => Ok(number.to_string()),
        Value::Integer(number) => Ok(number.to_string()),
        Value::Null | Value::Real(_) | Value::Text(_) | Value::Blob(_) => Err(invalid("real")),
    }
}

fn integer(value: &Value) -> Result<String, SqliteError> {
    match value {
        Value::Integer(number) => Ok(number.to_string()),
        Value::Null | Value::Real(_) | Value::Text(_) | Value::Blob(_) => Err(invalid("integer")),
    }
}

fn object(name: &str, value: Value) -> Result<String, SqliteError> {
    let Value::Text(text) = value else {
        return Err(invalid(name));
    };
    let parsed: wire::JsonObject =
        serde_json::from_str(&text).map_err(|source| SqliteError::StoredJson {
            column: "object",
            source,
        })?;
    serde_json::to_string(&parsed).map_err(|source| SqliteError::Encode {
        column: "object",
        source,
    })
}

fn text(value: Value) -> Result<String, SqliteError> {
    match value {
        Value::Text(text) => Ok(text),
        Value::Null | Value::Integer(_) | Value::Real(_) | Value::Blob(_) => Err(invalid("text")),
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(character),
        }
    }
    out
}

fn invalid(column: &str) -> SqliteError {
    SqliteError::Invalid {
        message: format!("stored {column} cannot be exported as Postgres COPY"),
    }
}
