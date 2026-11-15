use std::fmt::Formatter;

use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;
use serde::de::Deserializer;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde::ser::SerializeMap as _;
use serde::ser::SerializeSeq as _;
use sha2::Digest;
use sha2::Sha256;
use wire::JsonObject;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum OccurrenceKind {
    Tool,
    Assistant,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInput {
    arguments: JsonObject,
    spelling: String,
}

impl ToolInput {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[must_use]
    pub fn arguments(&self) -> &JsonObject {
        &self.arguments
    }

    #[must_use]
    pub fn spelling(&self) -> &str {
        &self.spelling
    }
}

#[cfg_attr(dylint_lib = "rr_lints", expect(customserde))]
impl<'de> Deserialize<'de> for ToolInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tree = Argument::deserialize(deserializer)?;
        let arguments = JsonObject::from_serialize(&tree).map_err(serde::de::Error::custom)?;
        let spelling = tree.spelling().map_err(serde::de::Error::custom)?;
        Ok(Self {
            arguments,
            spelling,
        })
    }
}

#[must_use]
pub fn digest_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn occurrence_digest(
    kind: OccurrenceKind,
    content: &str,
    tool_name: &str,
    arguments: Option<&ToolInput>,
) -> String {
    let spelling = arguments.map_or("", ToolInput::spelling);
    digest_text(&format!("{kind}\0{content}\0{tool_name}\0{spelling}"))
}

#[must_use]
pub fn occurrence_event_id(session_key: &str, digest: &str, occurrence: usize) -> String {
    format!("{session_key}:occ:{digest}:{occurrence}")
}

#[cfg_attr(dylint_lib = "rr_lints", expect(dynvalue))]
#[derive(Debug)]
enum Argument {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Text(String),
    List(Vec<Self>),
    Members(Vec<(String, Self)>),
}

#[cfg_attr(dylint_lib = "rr_lints", expect(customserde))]
impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ArgumentVisitor)
    }
}

#[cfg_attr(dylint_lib = "rr_lints", expect(customserde))]
impl Serialize for Argument {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(flag) => serializer.serialize_bool(*flag),
            Self::Signed(number) => serializer.serialize_i64(*number),
            Self::Unsigned(number) => serializer.serialize_u64(*number),
            Self::Float(number) => serializer.serialize_f64(*number),
            Self::Text(text) => serializer.serialize_str(text),
            Self::List(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Members(members) => {
                let mut map = serializer.serialize_map(Some(members.len()))?;
                for (key, member) in members {
                    map.serialize_entry(key, member)?;
                }
                map.end()
            }
        }
    }
}

struct ArgumentVisitor;

impl<'de> Visitor<'de> for ArgumentVisitor {
    type Value = Argument;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Argument::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Argument::Signed(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Argument::Unsigned(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(Argument::Float(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Argument::Text(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Argument::Text(value))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Argument::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = access.next_element()? {
            items.push(item);
        }
        Ok(Argument::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut members: Vec<(String, Argument)> = Vec::new();
        while let Some((key, value)) = access.next_entry::<String, Argument>()? {
            match members.iter_mut().find(|(name, _)| *name == key) {
                Some(slot) => slot.1 = value,
                None => members.push((key, value)),
            }
        }
        Ok(Argument::Members(members))
    }
}

impl Argument {
    #[expect(clippy::cast_precision_loss)]
    fn spelling(&self) -> Result<String, serde_json::Error> {
        Ok(match self {
            Self::Null => "null".to_owned(),
            Self::Bool(flag) => flag.to_string(),
            Self::Signed(number) => spell_number(*number as f64),
            Self::Unsigned(number) => spell_number(*number as f64),
            Self::Float(number) => spell_number(*number),
            Self::Text(text) => serde_json::to_string(text)?,
            Self::List(items) => {
                let spelled = items
                    .iter()
                    .map(Self::spelling)
                    .collect::<Result<Vec<_>, _>>()?;
                format!("[{}]", spelled.join(","))
            }
            Self::Members(members) => {
                let mut ordered: Vec<&(String, Self)> = members.iter().collect();
                ordered.sort_by_key(|(key, _)| (array_index(key).is_none(), array_index(key)));
                let spelled = ordered
                    .into_iter()
                    .map(|(key, member)| {
                        Ok(format!(
                            "{}:{}",
                            serde_json::to_string(key)?,
                            member.spelling()?
                        ))
                    })
                    .collect::<Result<Vec<_>, serde_json::Error>>()?;
                format!("{{{}}}", spelled.join(","))
            }
        })
    }
}

fn array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    if key.starts_with('0') {
        return None;
    }
    key.parse::<u32>().ok()
}

fn spell_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let scientific = format!("{value:e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return scientific;
    };
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-6..21).contains(&exponent) {
        positional(&digits, negative, exponent)
    } else {
        let sign = if exponent < 0 { "-" } else { "+" };
        format!("{mantissa}e{sign}{}", exponent.unsigned_abs())
    }
}

fn positional(digits: &str, negative: bool, exponent: i32) -> String {
    let sign = if negative { "-" } else { "" };
    let magnitude = exponent.unsigned_abs() as usize;
    if exponent < 0 {
        return format!("{sign}0.{}{digits}", "0".repeat(magnitude - 1));
    }
    let point = magnitude + 1;
    match digits.split_at_checked(point) {
        Some((whole, "")) => format!("{sign}{whole}"),
        Some((whole, rest)) => format!("{sign}{whole}.{rest}"),
        None => format!("{sign}{digits}{}", "0".repeat(point - digits.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_spell_like_the_recorded_hashes() {
        let cases = [
            ("0", "0"),
            ("-0.0", "0"),
            ("1.0", "1"),
            ("0.1", "0.1"),
            ("-1.5", "-1.5"),
            ("1e21", "1e+21"),
            ("-1e21", "-1e+21"),
            ("1e-7", "1e-7"),
            ("1.5e-7", "1.5e-7"),
            ("1e20", "100000000000000000000"),
            ("1e-6", "0.000001"),
            ("-1.5e-6", "-0.0000015"),
            ("12345678901234567890", "12345678901234567000"),
        ];
        for (input, expected) in cases {
            let number: f64 = input.parse().unwrap();
            assert_eq!(spell_number(number), expected, "of {input}");
        }
    }

    #[test]
    fn members_keep_document_order_and_integer_keys_sort_first() {
        let cases = [
            (r#"{"b":1,"2":2,"a":3}"#, r#"{"2":2,"b":1,"a":3}"#),
            (
                r#"{"9":null,"10":true,"01":false,"z":1}"#,
                r#"{"9":null,"10":true,"01":false,"z":1}"#,
            ),
            (
                r#"{"a":[1,{"b":null},true]}"#,
                r#"{"a":[1,{"b":null},true]}"#,
            ),
            (
                r#"{"s":"quote\"back\\slash\n\u0001é"}"#,
                r#"{"s":"quote\"back\\slash\n\u0001é"}"#,
            ),
            (r#"{"a":1,"a":2}"#, r#"{"a":2}"#),
        ];
        for (input, expected) in cases {
            assert_eq!(
                ToolInput::parse(input).unwrap().spelling(),
                expected,
                "of {input}"
            );
        }
    }

    #[test]
    fn arguments_keep_their_parsed_values() {
        let input = ToolInput::parse(r#"{"big":12345678901234567890,"n":-3,"f":1.5}"#).unwrap();
        assert_eq!(
            serde_json::to_string(input.arguments()).unwrap(),
            r#"{"big":12345678901234567890,"n":-3,"f":1.5}"#
        );
        assert!(ToolInput::parse("[1]").is_err());
        assert!(ToolInput::parse("{").is_err());
    }
}
