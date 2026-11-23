use rusqlite::types::Value;

use crate::Error;

#[derive(Debug, Clone, Copy, strum::EnumString)]
pub(super) enum Kind {
    #[strum(serialize = "text", serialize = "uuid", serialize = "date")]
    Text,
    #[strum(serialize = "json", serialize = "jsonb")]
    Json,
    #[strum(serialize = "_text")]
    TextArray,
    #[strum(serialize = "bool")]
    Bool,
    #[strum(serialize = "int2", serialize = "int4", serialize = "int8")]
    Integer,
    #[strum(serialize = "float4")]
    Single,
    #[strum(serialize = "float8")]
    Double,
    #[strum(serialize = "timestamptz")]
    Instant,
    #[strum(serialize = "bytea")]
    Bytes,
    #[strum(serialize = "halfvec")]
    Vector,
}

impl Kind {
    pub(super) fn parse(name: &str) -> Result<Self, Error> {
        name.parse()
            .map_err(|error| super::invalid(format!("unsupported transfer type {name}: {error}")))
    }

    pub(super) fn project(self, value: &str) -> String {
        match self {
            Self::TextArray => format!("to_json({value})::text"),
            Self::Instant => {
                format!("to_char({value} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
            }
            Self::Bytes => format!("encode({value}, 'hex')"),
            Self::Vector => format!("({value}::vector)::text"),
            Self::Text | Self::Json | Self::Bool | Self::Integer | Self::Single | Self::Double => {
                format!("{value}::text")
            }
        }
    }

    pub(super) fn cast(self, value: &str, cast: &str) -> String {
        match self {
            Self::TextArray => format!(
                "CASE WHEN {value} IS NOT NULL THEN
                ARRAY(SELECT jsonb_array_elements_text({value}::jsonb)) END"
            ),
            Self::Bytes => format!("decode({value}, 'hex')"),
            Self::Text
            | Self::Json
            | Self::Bool
            | Self::Integer
            | Self::Single
            | Self::Double
            | Self::Instant
            | Self::Vector => format!("{value}::{cast}"),
        }
    }

    pub(super) fn sqlite_read(self, value: &str) -> String {
        match self {
            Self::Instant => format!("rh_timestamp(rh_instant({value}))"),
            Self::Bytes => format!("CASE WHEN {value} IS NOT NULL THEN lower(hex({value})) END"),
            Self::Text | Self::Json | Self::TextArray | Self::Bool | Self::Integer => {
                format!("CAST({value} AS TEXT)")
            }
            Self::Vector | Self::Single | Self::Double => value.to_owned(),
        }
    }

    pub(super) fn compare(self, existing: &str, incoming: &str) -> String {
        match self {
            Self::Json | Self::TextArray => format!(
                "({existing} IS {incoming} OR
                ({existing} IS NOT NULL AND {incoming} IS NOT NULL AND
                NOT EXISTS (SELECT fullkey, type, atom FROM json_tree({existing})
                    EXCEPT SELECT fullkey, type, atom FROM json_tree({incoming})) AND
                NOT EXISTS (SELECT fullkey, type, atom FROM json_tree({incoming})
                    EXCEPT SELECT fullkey, type, atom FROM json_tree({existing}))))"
            ),
            Self::Instant => format!("rh_instant({existing}) IS rh_instant({incoming})"),
            Self::Single => format!(
                "abs({existing} - {incoming}) <= 1e-7 * max(abs({existing}), abs({incoming})) OR {existing} IS {incoming}"
            ),
            Self::Text | Self::Bool | Self::Integer | Self::Double | Self::Bytes | Self::Vector => {
                format!("{existing} IS {incoming}")
            }
        }
    }

    pub(super) fn bind(self, value: Option<&str>) -> Result<Value, rusqlite::Error> {
        let Some(value) = value else {
            return Ok(Value::Null);
        };
        Ok(match self {
            Self::Bool => Value::Integer(i64::from(value == "true" || value == "1")),
            Self::Bytes => Value::Blob(hex::decode(value).map_err(conversion)?),
            Self::Vector => Value::Blob(
                serde_json::from_str::<Vec<f32>>(value)
                    .map_err(conversion)?
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect(),
            ),
            Self::Integer => Value::Integer(value.parse().map_err(conversion)?),
            Self::Single | Self::Double => Value::Real(value.parse().map_err(conversion)?),
            Self::Text | Self::Json | Self::TextArray | Self::Instant => {
                Value::Text(value.to_owned())
            }
        })
    }
}

pub(super) fn vector(bytes: &[u8]) -> Result<String, rusqlite::Error> {
    let (chunks, padding) = bytes.as_chunks::<4>();
    if !padding.is_empty() {
        return Err(conversion(std::io::Error::other(
            "invalid embedding length",
        )));
    }
    let vector: Vec<_> = chunks.iter().copied().map(f32::from_le_bytes).collect();
    serde_json::to_string(&vector).map_err(conversion)
}

fn conversion(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}
