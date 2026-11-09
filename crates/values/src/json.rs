use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Extra(Map<String, Value>);

impl Extra {
    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct JsonObject(Map<String, Value>);

impl JsonObject {
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::to_value(value)?)
    }

    pub fn deserialize<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(Value::Object(self.0.clone()))
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    #[must_use]
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn insert<T: Serialize>(&mut self, key: &str, value: T) -> Result<(), serde_json::Error> {
        self.0.insert(key.to_owned(), serde_json::to_value(value)?);
        Ok(())
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.0.retain(|key, _| keep(key));
    }

    pub fn visit_strings_mut(&mut self, visit: &mut impl FnMut(&str, &mut String)) {
        for (key, value) in &mut self.0 {
            visit_strings(key, value, visit);
        }
    }

    pub fn scrub(&mut self, scrub: &mut impl Scrub) {
        scrub_members(&mut self.0, "", scrub);
    }
}

pub trait Scrub {
    fn is_secret_name(&self, key: &str) -> bool;
    fn key(&mut self, parent: &str, key: &mut String);
    fn text(&mut self, path: &str, text: &mut String);
    fn whole(&mut self, path: &str, current: Current<'_>) -> Option<String>;
    fn collision(&mut self, parent: &str, key: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current<'a> {
    Text(&'a str),
    Json(String),
}

fn scrub_members(members: &mut Map<String, Value>, parent: &str, scrub: &mut impl Scrub) {
    for (mut key, mut value) in std::mem::take(members) {
        let secret = scrub.is_secret_name(&key);
        scrub.key(parent, &mut key);
        let path = member_path(parent, &key);
        if secret {
            let current = match &value {
                Value::String(text) => Current::Text(text),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Current::Json(value.to_string()),
            };
            if let Some(placeholder) = scrub.whole(&path, current) {
                value = Value::String(placeholder);
            }
        } else {
            scrub_value(&mut value, &path, scrub);
        }
        if members.contains_key(&key) {
            let base = key.clone();
            let mut suffix = 2;
            while members.contains_key(&key) {
                key = format!("{base}#{suffix}");
                suffix += 1;
            }
            scrub.collision(parent, &key);
        }
        members.insert(key, value);
    }
}

fn scrub_value(value: &mut Value, path: &str, scrub: &mut impl Scrub) {
    match value {
        Value::String(text) => scrub.text(path, text),
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                scrub_value(item, &format!("{path}[{index}]"), scrub);
            }
        }
        Value::Object(members) => scrub_members(members, path, scrub),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn member_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

fn visit_strings(key: &str, value: &mut Value, visit: &mut impl FnMut(&str, &mut String)) {
    match value {
        Value::String(text) => visit(key, text),
        Value::Array(items) => {
            for item in items {
                visit_strings(key, item, visit);
            }
        }
        Value::Object(members) => {
            for (name, member) in members {
                visit_strings(name, member, visit);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_json;

    #[derive(Serialize, schemars::JsonSchema)]
    struct Sample {
        name: &'static str,
        count: u32,
    }

    #[test]
    fn objects_are_built_and_edited_through_serde() {
        let mut object = JsonObject::from_serialize(&Sample {
            name: "a",
            count: 2,
        })
        .unwrap();
        object.insert("flag", true).unwrap();
        object.retain(|key| key != "count");
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"name":"a","flag":true}"#
        );
        assert!(JsonObject::from_serialize(&[1, 2]).is_err());
    }

    fn sorted(value: Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            Value::Object(members) => {
                let mut entries: Vec<_> = members
                    .into_iter()
                    .map(|(name, member)| (name, sorted(member)))
                    .collect();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                Value::Object(entries.into_iter().collect())
            }
            leaf @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => leaf,
        }
    }

    #[derive(Serialize)]
    enum Shape {
        Unit,
        Newtype(JsonObject),
        Tuple(u8, String),
        Struct { zeta: Option<u8>, alpha: f64 },
    }

    #[derive(Serialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Tagged {
        Raw,
        Stamped { policy: String, count: u32 },
    }

    #[derive(Serialize)]
    struct Covered {
        version: u32,
        name: String,
        object: JsonObject,
        shapes: Vec<Shape>,
        tagged: Vec<Tagged>,
        missing: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped: Option<String>,
        bytes: Vec<u8>,
    }

    #[test]
    fn canonical_json_matches_sorting_a_parsed_value() {
        let object: JsonObject = serde_json::from_str(
            r#"{"z":[{"b":1,"a":[3,{"d":null,"c":true}]}],"é":"ü \" \\ \n \u0001","a":-1.5e300,"m":18446744073709551615,"":0}"#,
        )
        .unwrap();
        let sample = Covered {
            version: 2,
            name: "línea \u{1F600} \"quoted\"".to_owned(),
            object: object.clone(),
            shapes: vec![
                Shape::Unit,
                Shape::Newtype(object),
                Shape::Tuple(7, "x".to_owned()),
                Shape::Struct {
                    zeta: None,
                    alpha: 0.1,
                },
            ],
            tagged: vec![
                Tagged::Raw,
                Tagged::Stamped {
                    policy: "p".to_owned(),
                    count: 3,
                },
            ],
            missing: None,
            skipped: None,
            bytes: vec![0, 255],
        };
        let oracle = serde_json::to_vec(&sorted(serde_json::to_value(&sample).unwrap())).unwrap();
        assert_eq!(
            String::from_utf8(canonical_json(&sample).unwrap()).unwrap(),
            String::from_utf8(oracle).unwrap()
        );
    }

    #[test]
    fn canonical_json_writes_non_ascii_as_utf8_and_escapes_only_quotes_backslashes_and_controls() {
        assert_eq!(
            String::from_utf8(canonical_json("é \u{1F600} \" \\ \u{1} \u{7f}").unwrap()).unwrap(),
            "\"é \u{1F600} \\\" \\\\ \\u0001 \u{7f}\""
        );
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth_and_keeps_array_order() {
        let object: JsonObject =
            serde_json::from_str(r#"{ "b": [ {"z": 1, "y": 2}, 3 ], "a": null }"#).unwrap();
        assert_eq!(
            String::from_utf8(canonical_json(&object).unwrap()).unwrap(),
            r#"{"a":null,"b":[{"y":2,"z":1},3]}"#
        );
    }

    #[test]
    fn string_leaves_are_visited_with_their_member_name() {
        let mut object: JsonObject =
            serde_json::from_str(r#"{"a":"x","list":["y",{"b":"z"}],"n":1,"none":null}"#).unwrap();
        let mut seen = Vec::new();
        object.visit_strings_mut(&mut |key, text| {
            seen.push(format!("{key}={text}"));
            text.push('!');
        });
        assert_eq!(seen, ["a=x", "list=y", "b=z"]);
        assert_eq!(
            serde_json::to_string(&object).unwrap(),
            r#"{"a":"x!","list":["y!",{"b":"z!"}],"n":1,"none":null}"#
        );
    }
}
